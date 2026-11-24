//! 装配层：配置 → 渠道/来源实例的转换集中在这里（通知渠道、QQ 凭据、
//! target 格式等渠道知识只允许出现在本模块）。附属的调度/tracker/QQ 命令
//! 注册走 modules::MODULES 注册表（各模块 DEF 自描述），不在此登记。

use crate::{
    adapters::vault::GitVault,
    app::obsidian::ObsidianSectionExt,
    bootstrap::config::Config,
    channels::{
        feishu::{self, FeishuWebhookNotifier},
        qq::{QqNotifier, ReplySequencer, api::QqApiClient},
        telegram::{self, TelegramNotifier, api::TelegramApi},
        unseal::UnsealingChannel,
    },
    domain::NotifyRoute,
    ingress::{
        cli::QqCredentials,
        schedule::{PollJob, ScheduledJob},
    },
    ports::{
        channel::NotifyChannel,
        commands::{CommandHandler, ImageConsumer},
        storage::TaskStore,
        vault::Vault,
    },
    runtime::proactive::ProactiveDispatcher,
};
use anyhow::{Context, Result};
use std::sync::Arc;

/// 终态通知渠道装配：QQ 网关进程传入 api + 共享序号器时注册 QQ；配置了飞书 webhook 即注册飞书。
/// qq_seq 必须与 QqBot 共享同一实例，保证同 msg_id 被动回复 seq 不重复。
///
/// 每个通道都被 [`UnsealingChannel`] 包一层：**出站前把消息里的凭据标识换成真值**
/// （`cred_xxx` → 明文）。LLM 上下文里只有标识，真值在网关出站前才落地 ——
/// 模型和模型供应商的日志里没有明文，用户收到的却是能直接用的。
pub fn notify_channels(
    config: &Config,
    store: Arc<dyn TaskStore>,
    qq: Option<Arc<QqApiClient>>,
    qq_seq: Option<Arc<ReplySequencer>>,
) -> Result<Vec<Arc<dyn NotifyChannel>>> {
    let mut channels: Vec<Arc<dyn NotifyChannel>> = Vec::new();
    if let Some(api) = qq {
        let sequencer = qq_seq.unwrap_or_else(|| Arc::new(ReplySequencer::default()));
        channels.push(Arc::new(
            QqNotifier::new(api, sequencer)
                .with_default_target(config.channel.qq.default_notify_target.clone()),
        ));
    }
    if let Some(url) = feishu_webhook_url(config) {
        channels.push(Arc::new(FeishuWebhookNotifier::new(url)));
    }
    if let Some(token) = telegram_token(config) {
        channels.push(Arc::new(TelegramNotifier::new(
            Arc::new(telegram_api(config, token)),
            config.channel.telegram.default_chat_id.clone(),
        )));
    }
    // 一律包解封层：所有出站文本都要过一遍（入站回复/终态通知/主动推送/提醒）。
    Ok(channels
        .into_iter()
        .map(|channel| {
            Arc::new(UnsealingChannel::new(channel, store.clone())) as Arc<dyn NotifyChannel>
        })
        .collect())
}

/// Telegram 客户端：token + 该渠道专属出网代理（境内直连不通）。
pub fn telegram_api(config: &Config, token: String) -> TelegramApi {
    TelegramApi::with_proxy(token, config.channel.telegram.proxy_url.as_deref())
}

/// 注册各业务模块提供的扩展命令；这是唯一同时认识渠道与业务模块的位置。
/// 模块内的命令处理器按契约只用 `ctx.reply_channel`，因此同一批处理器
/// 可以原样注册给 QQ 与 Telegram。
pub fn channel_command_handlers(
    config: &Config,
    backend: &str,
) -> Result<Vec<Arc<dyn CommandHandler>>> {
    // 注册表模块的命令先行收集（数据驱动）。
    let mut handlers: Vec<Arc<dyn CommandHandler>> = Vec::new();
    for def in crate::modules::sorted() {
        handlers.extend((def.qq_handlers)(config, backend)?);
    }
    Ok(handlers)
}

/// QQ 图片消费者装配：各模块经 DEF.image_consumers 自报（如学习通二维码
/// 照片签到），装配层不认识任何具体业务。
pub fn qq_image_consumers(config: &Config) -> Result<Vec<Arc<dyn ImageConsumer>>> {
    let mut consumers: Vec<Arc<dyn ImageConsumer>> = Vec::new();
    for def in crate::modules::sorted() {
        consumers.extend((def.image_consumers)(config)?);
    }
    Ok(consumers)
}

/// 飞书 webhook URL：环境变量 RU41_FEISHU_WEBHOOK_URL > 配置文件；空串视为未配置。
fn feishu_webhook_url(config: &Config) -> Option<String> {
    std::env::var("RU41_FEISHU_WEBHOOK_URL")
        .ok()
        .or_else(|| config.channel.feishu.webhook_url.clone())
        .filter(|url| !url.trim().is_empty())
}

/// Telegram bot token：环境变量 RU41_TELEGRAM_BOT_TOKEN > 配置文件；空串视为未配置。
pub fn telegram_token(config: &Config) -> Option<String> {
    std::env::var("RU41_TELEGRAM_BOT_TOKEN")
        .ok()
        .or_else(|| config.channel.telegram.bot_token.clone())
        .filter(|token| !token.trim().is_empty())
}

/// Telegram 身份绑定口令：环境变量 RU41_TELEGRAM_BIND_SECRET > 配置文件。
pub fn telegram_bind_secret(config: &Config) -> Option<String> {
    std::env::var("RU41_TELEGRAM_BIND_SECRET")
        .ok()
        .or_else(|| config.channel.telegram.bind_secret.clone())
        .filter(|secret| !secret.trim().is_empty())
}

/// Telegram 用户白名单：为空表示不限制。
pub fn telegram_allowed_user_ids(config: &Config) -> Vec<i64> {
    config
        .channel
        .telegram
        .allowed_user_ids
        .clone()
        .unwrap_or_default()
}

/// 默认通知出口：渠道名 → 路由的转换只在装配层做，业务模块只拿到解析好的路由。
pub fn default_notify_route(config: &Config) -> Result<Option<NotifyRoute>> {
    let Some(channel) = config.notify.channel.as_deref() else {
        return Ok(None);
    };
    let route = match channel {
        "feishu" => {
            feishu_webhook_url(config).context(
                "feishu webhook required: RU41_FEISHU_WEBHOOK_URL or channel.feishu.webhook_url",
            )?;
            NotifyRoute {
                channel: "feishu".into(),
                target: feishu::DEFAULT_TARGET.into(),
                msg_id: None,
            }
        }
        "qq" => NotifyRoute {
            channel: "qq".into(),
            target: config
                .channel
                .qq
                .default_notify_target
                .clone()
                .context("qq notify requires channel.qq.default_notify_target")?,
            msg_id: None,
        },
        "telegram" => {
            telegram_token(config).context(
                "telegram bot token required: RU41_TELEGRAM_BOT_TOKEN or channel.telegram.bot_token",
            )?;
            config
                .channel
                .telegram
                .default_chat_id
                .clone()
                .filter(|chat| !chat.trim().is_empty())
                .context("telegram notify requires channel.telegram.default_chat_id")?;
            NotifyRoute {
                channel: "telegram".into(),
                target: telegram::DEFAULT_TARGET.into(),
                msg_id: None,
            }
        }
        other => anyhow::bail!("unknown notify channel: {other}"),
    };
    Ok(Some(route))
}

/// 来源通知开关的统一语义：默认开启，显式 notify = false 时只入队不推送。
pub fn source_notify(config: &Config, enabled: Option<bool>) -> Result<Option<NotifyRoute>> {
    if enabled.unwrap_or(true) {
        default_notify_route(config)
    } else {
        Ok(None)
    }
}

/// 追踪器装配共用的通知对：开启时解析具体通知器与目标，关闭时为 (None, 空目标)。
pub fn notify_pair(
    config: &Config,
    enabled: Option<bool>,
) -> Result<(Option<Arc<dyn NotifyChannel>>, String)> {
    match source_notify(config, enabled)? {
        Some(route) => {
            let notifier = default_notifier(config, &route.channel)?;
            Ok((Some(notifier), route.target))
        }
        None => Ok((None, String::new())),
    }
}

/// Obsidian Vault 句柄：开关打开且 [obsidian] 已配置时才接线，否则不同步。
pub fn vault_if_ready(config: &Config, enabled: bool) -> Result<Option<Arc<dyn Vault>>> {
    if enabled && config.obsidian.is_ready() {
        Ok(Some(vault_client(&config.obsidian.resolved(None)?)))
    } else {
        Ok(None)
    }
}

/// 按 [notify] 渠道构造具体通知器；渠道知识只存在于装配层。
pub fn default_notifier(config: &Config, channel: &str) -> Result<Arc<dyn NotifyChannel>> {
    match channel {
        "feishu" => Ok(Arc::new(FeishuWebhookNotifier::new(
            feishu_webhook_url(config).context(
                "feishu webhook required: RU41_FEISHU_WEBHOOK_URL or channel.feishu.webhook_url",
            )?,
        ))),
        "qq" => Ok(Arc::new(
            QqNotifier::new(
                Arc::new(qq_api(
                    &QqCredentials {
                        app_id: None,
                        app_secret: None,
                    },
                    config,
                )?),
                // 跟踪器进程只发主动消息，序号器进程内独立即可。
                Arc::new(ReplySequencer::default()),
            )
            .with_default_target(config.channel.qq.default_notify_target.clone()),
        )),
        "telegram" => Ok(Arc::new(TelegramNotifier::new(
            Arc::new(telegram_api(
                config,
                telegram_token(config).context(
                    "telegram bot token required: RU41_TELEGRAM_BOT_TOKEN or channel.telegram.bot_token",
                )?,
            )),
            config.channel.telegram.default_chat_id.clone(),
        ))),
        other => anyhow::bail!("unknown notify channel: {other}"),
    }
}

/// 知识库索引器：显式配置 [knowledge].db_path 才接线。
/// 索引是衍生数据，路径必须落到共享数据卷（obsidian 写、worker 读）。
pub fn knowledge_service(
    config: &Config,
) -> Result<Option<Arc<crate::app::knowledge::KnowledgeService>>> {
    let Some(db) = &config.knowledge.db_path else {
        return Ok(None);
    };
    let cache = config.knowledge.cache_dir.clone().unwrap_or_else(|| {
        db.parent()
            .map(|d| d.join("fastembed"))
            .unwrap_or_else(|| "fastembed".into())
    });
    let embedder = Arc::new(crate::adapters::embedding::fastembed::FastembedEmbedder::new(cache)?);
    Ok(Some(Arc::new(
        crate::app::knowledge::KnowledgeService::new(embedder, db.clone())?,
    )))
}

/// Vault git 客户端：渠道无关的写通道，QQ 命令与核心服务共用同一实现配置。
pub fn vault_client(
    config: &crate::app::obsidian::ObsidianConfig,
) -> Arc<dyn crate::ports::vault::Vault> {
    Arc::new(GitVault::new(
        config.vault_path.clone(),
        config.remote.clone(),
        config.branch.clone(),
    ))
}

/// 定时任务注册表：全部来自 modules::MODULES 注册表的数据驱动汇总；
/// 新附属在模块内声明 DEF 即自动出现在 schedule list，无需改这里。
pub fn scheduled_jobs(config: &Config) -> Vec<ScheduledJob> {
    crate::modules::sorted()
        .into_iter()
        .flat_map(|def| {
            (def.schedule)(config)
                .into_iter()
                .map(move |js| js.to_scheduled_job(def.name))
        })
        .collect()
}

/// 按任务名构造对应追踪器（papers/rss/ecard/zhihuishu）；未登记的名字返回 Ok(None)。
/// schedule run 与各模块 run 命令共用这一份构造，入口侧不再重复 match。
pub fn tracker_job(
    config: &Config,
    store: Arc<dyn TaskStore>,
    name: &str,
) -> Result<Option<Box<dyn PollJob>>> {
    // 注册表优先：已迁移模块数据驱动装配；未命中再走老的手写注册。
    for def in crate::modules::sorted() {
        if let Some(job) = (def.tracker)(config, store.clone(), name)? {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

/// 待办提醒的通知出口解析：显式 target 优先，否则取 [notify] 默认出口。
pub fn todo_notify_route(
    config: &Config,
    channel: String,
    target: Option<String>,
) -> Result<NotifyRoute> {
    match target {
        Some(target) => Ok(NotifyRoute {
            channel,
            target,
            msg_id: None,
        }),
        None => default_notify_route(config)?
            .context("todo remind requires --target or a default notify channel"),
    }
}

/// 按已解析路由取具体通知器；渠道知识只存在于装配层，入口侧不做渠道判断。
pub fn notifier_for_route(config: &Config, route: &NotifyRoute) -> Result<Arc<dyn NotifyChannel>> {
    default_notifier(config, &route.channel)
}

/// 主动推送的投递器：主渠道 + `[notify].fanout` 里的并行渠道。
///
/// 提醒类消息（到点提醒、行程提醒、错过的课程提醒）走这里——
/// 它们不经过 dispatcher，只用 `default_notifier` 就只到主渠道，
/// 用户的「双网关推送」预期就落空了。
pub fn proactive_dispatcher(
    config: &Config,
    store: Arc<dyn TaskStore>,
) -> Result<ProactiveDispatcher> {
    // QQ 凭据缺失不算致命：容器没带 QQ 凭据时只发并行渠道。
    let qq_api = qq_api(
        &QqCredentials {
            app_id: None,
            app_secret: None,
        },
        config,
    )
    .ok();
    let channels = notify_channels(config, store.clone(), qq_api.map(Arc::new), None)?;
    Ok(ProactiveDispatcher::new(
        store,
        channels,
        config.notify.fanout.clone().unwrap_or_default(),
    ))
}

pub fn qq_api(credentials: &QqCredentials, config: &Config) -> Result<QqApiClient> {
    let app_id = credentials
        .app_id
        .clone()
        .or_else(|| config.channel.qq.app_id.clone())
        .context("QQ app id required: --qq-app-id, RU41_QQ_APP_ID or channel.qq.app_id")?;
    let app_secret = credentials
        .app_secret
        .clone()
        .or_else(|| config.channel.qq.app_secret.clone())
        .context(
            "QQ app secret required: --qq-app-secret, RU41_QQ_APP_SECRET or channel.qq.app_secret",
        )?;
    Ok(QqApiClient::new(app_id, app_secret))
}
