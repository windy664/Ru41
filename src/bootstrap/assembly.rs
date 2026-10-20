//! 附属装配层：配置 → 渠道/来源实例的全部转换集中在这里。
//! 渠道知识（飞书 webhook、QQ 凭据、target 格式）只允许出现在本模块，
//! 业务模块只拿到解析好的路由与追踪器；新增附属时在此登记装配函数。

use crate::{
    adapters::sources::feed::FeedClient,
    adapters::vault::GitVault,
    app::obsidian::ObsidianService,
    bootstrap::config::Config,
    channels::{
        feishu::{self, FeishuWebhookNotifier},
        qq::{QqNotifier, api::QqApiClient},
    },
    domain::NotifyRoute,
    ingress::{
        cli::QqCredentials,
        schedule::{PollJob, ScheduledJob},
    },
    modules::{
        blog::{BlogPublishTracker, BlogSection},
        chaoxing::{ChaoxingClient, ChaoxingSection, ChaoxingTracker, ChaoxingWorkTracker},
        ecard::{EcardClient, EcardQqCommand, EcardSection, EcardTracker},
        jwxt::{JwxtClient, JwxtQqCommand, JwxtSection, JwxtTracker},
        obsidian::ObsidianQqCommand,
        papers::{ArxivClient, ArxivSection, PaperTracker, PapersQqCommand},
        planner::TodoQqCommand,
        rss::{RssSection, RssTracker},
        zhihuishu::{ZhihuishuQqCommand, ZhihuishuSection, ZhihuishuTracker},
    },
    ports::{channel::NotifyChannel, commands::CommandHandler, storage::TaskStore, vault::Vault},
};
use anyhow::{Context, Result};
use std::sync::Arc;

/// 终态通知渠道装配：QQ 网关进程传入 api 时注册 QQ；配置了飞书 webhook 即注册飞书。
pub fn notify_channels(
    config: &Config,
    qq: Option<Arc<QqApiClient>>,
) -> Result<Vec<Arc<dyn NotifyChannel>>> {
    let mut channels: Vec<Arc<dyn NotifyChannel>> = Vec::new();
    if let Some(api) = qq {
        channels.push(Arc::new(QqNotifier::new(api)));
    }
    if let Some(url) = feishu_webhook_url(config) {
        channels.push(Arc::new(FeishuWebhookNotifier::new(url)));
    }
    Ok(channels)
}

/// 注册各业务模块提供的 QQ 扩展命令；这是唯一同时认识 QQ 与业务模块的位置。
pub fn qq_command_handlers(config: &Config, backend: &str) -> Result<Vec<Arc<dyn CommandHandler>>> {
    let mut handlers: Vec<Arc<dyn CommandHandler>> = vec![
        Arc::new(PapersQqCommand::new(
            ArxivClient::new(),
            config.sources.arxiv.interests.clone().unwrap_or_default(),
            backend.to_owned(),
        )),
        Arc::new(EcardQqCommand::new(backend.to_owned())),
        Arc::new(JwxtQqCommand::new(
            config
                .sources
                .jwxt
                .semester_start
                .as_deref()
                .and_then(|raw| chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()),
        )),
        Arc::new(TodoQqCommand::new(config.todo.options()?)),
        Arc::new(ZhihuishuQqCommand::new(
            config
                .sources
                .zhihuishu
                .resolved(String::new())?
                .report_path,
        )),
    ];
    // Obsidian 未配置 vault_path 时不注册 /记一下，避免半配置状态下写空气。
    if config.obsidian.is_ready() {
        let resolved = config.obsidian.resolved(None)?;
        handlers.push(Arc::new(ObsidianQqCommand::new(
            vault_client(&resolved),
            resolved.inbox_dir.clone(),
            config.todo.options()?.offset_seconds,
        )));
    }
    Ok(handlers)
}

/// 飞书 webhook URL：环境变量 RU41_FEISHU_WEBHOOK_URL > 配置文件；空串视为未配置。
fn feishu_webhook_url(config: &Config) -> Option<String> {
    std::env::var("RU41_FEISHU_WEBHOOK_URL")
        .ok()
        .or_else(|| config.channel.feishu.webhook_url.clone())
        .filter(|url| !url.trim().is_empty())
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
        other => anyhow::bail!("unknown notify channel: {other}"),
    };
    Ok(Some(route))
}

/// 来源通知开关的统一语义：默认开启，显式 notify = false 时只入队不推送。
fn source_notify(config: &Config, enabled: Option<bool>) -> Result<Option<NotifyRoute>> {
    if enabled.unwrap_or(true) {
        default_notify_route(config)
    } else {
        Ok(None)
    }
}

/// 由 [sources.arxiv] 配置组装追踪器；通知路由来自全局 [notify] 出口。
pub fn paper_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<PaperTracker> {
    let arxiv = &config.sources.arxiv;
    Ok(PaperTracker::new(
        store,
        ArxivClient::new(),
        arxiv.resolved(source_notify(config, arxiv.notify)?)?,
    ))
}

/// 由 [sources.rss] 配置组装追踪器；通知路由同样来自全局 [notify] 出口。
pub fn rss_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<RssTracker> {
    let rss = &config.sources.rss;
    Ok(RssTracker::new(
        store,
        FeedClient::new(),
        rss.resolved(source_notify(config, rss.notify)?)?,
    ))
}

/// 按 [notify] 渠道构造具体通知器；渠道知识只存在于装配层。
fn default_notifier(config: &Config, channel: &str) -> Result<Arc<dyn NotifyChannel>> {
    match channel {
        "feishu" => Ok(Arc::new(FeishuWebhookNotifier::new(
            feishu_webhook_url(config).context(
                "feishu webhook required: RU41_FEISHU_WEBHOOK_URL or channel.feishu.webhook_url",
            )?,
        ))),
        "qq" => Ok(Arc::new(QqNotifier::new(Arc::new(qq_api(
            &QqCredentials {
                app_id: None,
                app_secret: None,
            },
            config,
        )?)))),
        other => anyhow::bail!("unknown notify channel: {other}"),
    }
}

/// 由 [sources.ecard] 配置组装水电监控；提醒渠道同样来自全局 [notify] 出口。
pub fn ecard_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<EcardTracker> {
    let ecard = &config.sources.ecard;
    let base_url = ecard
        .base_url
        .clone()
        .context("sources.ecard.base_url required")?;
    let (notifier, notify_target) = match source_notify(config, ecard.notify)? {
        Some(route) => (
            Some(default_notifier(config, &route.channel)?),
            route.target,
        ),
        None => (None, String::new()),
    };
    Ok(EcardTracker::new(
        store,
        EcardClient::new(base_url),
        notifier,
        ecard.resolved(notify_target)?,
    ))
}

/// 由 [sources.chaoxing] 配置组装学习通签到监控；协议端直连，无 sidecar。
pub fn chaoxing_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<ChaoxingTracker> {
    let chaoxing = &config.sources.chaoxing;
    let (notifier, notify_target) = match source_notify(config, chaoxing.notify)? {
        Some(route) => (
            Some(default_notifier(config, &route.channel)?),
            route.target,
        ),
        None => (None, String::new()),
    };
    let resolved = chaoxing.resolved(notify_target)?;
    let client = ChaoxingClient::new(
        resolved.phone.clone(),
        resolved.password.clone(),
        resolved.cookie_path.clone(),
    );
    Ok(ChaoxingTracker::new(store, client, notifier, resolved))
}

/// 由 [sources.jwxt] 配置组装教务课表监控；CAS 单点登录，TGT 持久化。
pub fn jwxt_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<JwxtTracker> {
    let jwxt = &config.sources.jwxt;
    let (notifier, notify_target) = match source_notify(config, jwxt.notify)? {
        Some(route) => (
            Some(default_notifier(config, &route.channel)?),
            route.target,
        ),
        None => (None, String::new()),
    };
    let resolved = jwxt.resolved(notify_target)?;
    let client = JwxtClient::new(
        resolved.username.clone(),
        resolved.password.clone(),
        resolved.cas_base.clone(),
        resolved.cas_modulus.clone(),
        resolved.cas_exponent.clone(),
        resolved.jwxt_base.clone(),
        resolved.session_path.clone(),
    )
    .with_browser_ua();
    Ok(JwxtTracker::new(store, client, notifier, resolved))
}

/// 由 [sources.zhihuishu] 配置组装智慧树作业提醒；报告文件由 zhihuishu-sidecar 产出。
pub fn zhihuishu_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<ZhihuishuTracker> {
    let zhihuishu = &config.sources.zhihuishu;
    let (notifier, notify_target) = match source_notify(config, zhihuishu.notify)? {
        Some(route) => (
            Some(default_notifier(config, &route.channel)?),
            route.target,
        ),
        None => (None, String::new()),
    };
    // Obsidian 课程同步：[obsidian] 配好 Vault 才启用；经核心 Vault 端口读写
    let vault: Option<Arc<dyn Vault>> =
        if zhihuishu.resolved(notify_target.clone())?.vault_sync && config.obsidian.is_ready() {
            let resolved = config.obsidian.resolved(None)?;
            Some(Arc::new(GitVault::new(
                resolved.vault_path.clone(),
                resolved.remote.clone(),
                resolved.branch.clone(),
            )))
        } else {
            None
        };
    Ok(ZhihuishuTracker::new(
        store,
        notifier,
        zhihuishu.resolved(notify_target)?,
        config.todo.options()?,
        vault,
    ))
}

/// 学习通作业监控：复用 [sources.chaoxing] 凭证与 cookie 会话；低频独立调度。
/// Obsidian 同步按 work_vault_sync 开关走 [obsidian] 的 Vault；待办走全局 TodoService。
pub fn chaoxing_work_tracker(
    config: &Config,
    store: Arc<dyn TaskStore>,
) -> Result<ChaoxingWorkTracker> {
    let chaoxing = &config.sources.chaoxing;
    let (notifier, notify_target) = match source_notify(config, chaoxing.notify)? {
        Some(route) => (
            Some(default_notifier(config, &route.channel)?),
            route.target,
        ),
        None => (None, String::new()),
    };
    let resolved = chaoxing.resolved(notify_target.clone())?;
    let vault: Option<Arc<dyn Vault>> = if resolved.work_vault_sync && config.obsidian.is_ready() {
        let obsidian = config.obsidian.resolved(None)?;
        Some(Arc::new(GitVault::new(
            obsidian.vault_path.clone(),
            obsidian.remote.clone(),
            obsidian.branch.clone(),
        )))
    } else {
        None
    };
    Ok(ChaoxingWorkTracker::new(
        store,
        ChaoxingClient::new(
            resolved.phone.clone(),
            resolved.password.clone(),
            resolved.cookie_path.clone(),
        ),
        notifier,
        resolved,
        config.todo.options()?,
        vault,
    ))
}

/// 由 [obsidian] 配置组装 Vault 核心服务；整理完成通知来自全局 [notify] 出口。
pub fn obsidian_service(config: &Config, store: Arc<dyn TaskStore>) -> Result<ObsidianService> {
    let obsidian = &config.obsidian;
    let route = source_notify(config, obsidian.notify)?;
    let notifier = match &route {
        Some(route) => Some(default_notifier(config, &route.channel)?),
        None => None,
    };
    let resolved = obsidian.resolved(route)?;
    Ok(ObsidianService::new(
        store,
        vault_client(&resolved),
        notifier,
        resolved,
    ))
}

/// Vault git 客户端：渠道无关的写通道，QQ 命令与核心服务共用同一实现配置。
fn vault_client(
    config: &crate::app::obsidian::ObsidianConfig,
) -> Arc<dyn crate::ports::vault::Vault> {
    Arc::new(GitVault::new(
        config.vault_path.clone(),
        config.remote.clone(),
        config.branch.clone(),
    ))
}

/// 由 [sources.blog] 组装博客发布管线；Vault 句柄复用核心 [obsidian] 的连接配置。
pub fn blog_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<BlogPublishTracker> {
    let blog = &config.sources.blog;
    anyhow::ensure!(
        config.obsidian.is_ready(),
        "sources.blog requires obsidian.vault_path (posts live in the vault repo)"
    );
    let vault_cfg = config.obsidian.resolved(None)?;
    let route = source_notify(config, blog.notify)?;
    let notifier = match &route {
        Some(route) => Some(default_notifier(config, &route.channel)?),
        None => None,
    };
    let resolved = blog.resolved(route)?;
    Ok(BlogPublishTracker::new(
        store,
        vault_client(&vault_cfg),
        Arc::new(GitVault::new(
            resolved.repo_path.clone(),
            resolved.remote.clone(),
            resolved.branch.clone(),
        )),
        notifier,
        resolved,
    ))
}

/// 定时任务注册表：汇总各模块登记的轮询任务、间隔与启停状态。
/// 这是唯一同时认识全部模块与调度核心入口的位置；新附属在此追加条目。
pub fn scheduled_jobs(config: &Config) -> Vec<ScheduledJob> {
    let sources = &config.sources;
    let arxiv = &sources.arxiv;
    let rss = &sources.rss;
    let ecard = &sources.ecard;
    let todo_route_ready = default_notify_route(config).ok().flatten().is_some();
    vec![
        ScheduledJob {
            module: "papers".into(),
            name: "papers".into(),
            interval_secs: Some(
                arxiv
                    .interval_secs
                    .unwrap_or(ArxivSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "papers poll".into(),
            run_command: "papers run".into(),
            enabled: arxiv.categories.as_ref().is_some_and(|c| !c.is_empty()),
            notes: (!arxiv.categories.as_ref().is_some_and(|c| !c.is_empty()))
                .then(|| "配置 sources.arxiv.categories 后启用".into()),
        },
        ScheduledJob {
            module: "rss".into(),
            name: "rss".into(),
            interval_secs: Some(
                rss.interval_secs
                    .unwrap_or(RssSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "rss poll".into(),
            run_command: "rss run".into(),
            enabled: rss.feeds.as_ref().is_some_and(|f| !f.is_empty()),
            notes: (!rss.feeds.as_ref().is_some_and(|f| !f.is_empty()))
                .then(|| "配置 sources.rss.feeds 后启用".into()),
        },
        ScheduledJob {
            module: "ecard".into(),
            name: "ecard".into(),
            interval_secs: Some(
                ecard
                    .interval_secs
                    .unwrap_or(EcardSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "ecard poll".into(),
            run_command: "ecard run".into(),
            enabled: ecard.is_ready(),
            notes: (!ecard.is_ready())
                .then(|| "需 sources.ecard 房间信息及 token（或 RU41_ECARD_TOKEN）".into()),
        },
        ScheduledJob {
            module: "chaoxing".into(),
            name: "chaoxing".into(),
            interval_secs: Some(
                sources
                    .chaoxing
                    .interval_secs
                    .unwrap_or(ChaoxingSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "chaoxing poll".into(),
            run_command: "chaoxing run".into(),
            enabled: sources.chaoxing.is_ready(),
            notes: (!sources.chaoxing.is_ready()).then(|| {
                "需 sources.chaoxing 手机号与密码（或 RU41_XXT_PHONE/RU41_XXT_PASSWORD）".into()
            }),
        },
        ScheduledJob {
            module: "chaoxing".into(),
            name: "chaoxing-work".into(),
            interval_secs: Some(
                sources
                    .chaoxing
                    .work_poll_secs
                    .unwrap_or(ChaoxingSection::DEFAULT_WORK_POLL_SECS),
            ),
            poll_command: "chaoxing work".into(),
            run_command: "chaoxing work".into(),
            enabled: sources.chaoxing.is_ready(),
            notes: (!sources.chaoxing.is_ready())
                .then(|| "随 sources.chaoxing 凭证启用；作业轮询独立低频（默认 30 分钟）".into()),
        },
        ScheduledJob {
            module: "jwxt".into(),
            name: "jwxt".into(),
            interval_secs: Some(
                sources
                    .jwxt
                    .interval_secs
                    .unwrap_or(JwxtSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "jwxt poll".into(),
            run_command: "jwxt run".into(),
            enabled: sources.jwxt.is_ready(),
            notes: if !sources.jwxt.is_ready() {
                Some(
                    "需 sources.jwxt 用户名与密码（或 RU41_JWXT_USERNAME/RU41_JWXT_PASSWORD）"
                        .into(),
                )
            } else if sources.jwxt.semester_start.is_none() {
                Some("已启用；配置 semester_start（学期第一周周一）后才有明日课表提醒".into())
            } else {
                None
            },
        },
        ScheduledJob {
            module: "zhihuishu".into(),
            name: "zhihuishu".into(),
            interval_secs: Some(
                sources
                    .zhihuishu
                    .interval_secs
                    .unwrap_or(ZhihuishuSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "zhihuishu poll".into(),
            run_command: "zhihuishu run".into(),
            enabled: true,
            notes: None,
        },
        ScheduledJob {
            module: "blog".into(),
            name: "blog".into(),
            interval_secs: Some(
                sources
                    .blog
                    .interval_secs
                    .unwrap_or(BlogSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "blog poll".into(),
            run_command: "blog run".into(),
            enabled: sources.blog.is_ready() && config.obsidian.is_ready(),
            notes: (!sources.blog.is_ready())
                .then(|| "配置 sources.blog.repo_path 后启用".into())
                .or_else(|| {
                    (!config.obsidian.is_ready())
                        .then(|| "依赖 obsidian.vault_path（待发布文件在 Vault 仓库）".into())
                }),
        },
        ScheduledJob {
            module: "obsidian".into(),
            name: "obsidian".into(),
            interval_secs: Some(
                config
                    .obsidian
                    .interval_secs
                    .unwrap_or(crate::bootstrap::config::ObsidianSection::DEFAULT_INTERVAL_SECS),
            ),
            poll_command: "obsidian poll".into(),
            run_command: "obsidian run".into(),
            enabled: config.obsidian.is_ready(),
            notes: (!config.obsidian.is_ready()).then(|| "配置 obsidian.vault_path 后启用".into()),
        },
        ScheduledJob {
            module: "planner".into(),
            name: "todo-remind".into(),
            interval_secs: Some(60),
            poll_command: "todo remind --once".into(),
            run_command: "todo remind".into(),
            enabled: todo_route_ready,
            notes: if todo_route_ready {
                Some("间隔可用 --interval-secs 覆盖".into())
            } else {
                Some("需 [notify] 默认出口，或 schedule run --target 显式指定".into())
            },
        },
    ]
}

/// 按任务名构造对应追踪器（papers/rss/ecard/zhihuishu）；未登记的名字返回 Ok(None)。
/// schedule run 与各模块 run 命令共用这一份构造，入口侧不再重复 match。
pub fn tracker_job(
    config: &Config,
    store: Arc<dyn TaskStore>,
    name: &str,
) -> Result<Option<Box<dyn PollJob>>> {
    Ok(Some(match name {
        "papers" => Box::new(paper_tracker(config, store)?),
        "rss" => Box::new(rss_tracker(config, store)?),
        "ecard" => Box::new(ecard_tracker(config, store)?),
        "chaoxing" => Box::new(chaoxing_tracker(config, store)?),
        "chaoxing-work" => Box::new(chaoxing_work_tracker(config, store)?),
        "jwxt" => Box::new(jwxt_tracker(config, store)?),
        "zhihuishu" => Box::new(zhihuishu_tracker(config, store)?),
        "obsidian" => Box::new(obsidian_service(config, store)?),
        "blog" => Box::new(blog_tracker(config, store)?),
        _ => return Ok(None),
    }))
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
