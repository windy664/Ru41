//! 附属装配层：配置 → 渠道/来源实例的全部转换集中在这里。
//! 渠道知识（飞书 webhook、QQ 凭据、target 格式）只允许出现在本模块，
//! 业务模块只拿到解析好的路由与追踪器；新增附属时在此登记装配函数。

use crate::{
    adapters::sources::{
        arxiv::ArxivClient,
        ecard::{EcardClient, EcardRoom},
        feed::FeedClient,
    },
    app::{
        ecard::{EcardConfig, EcardTracker},
        papers::{PaperTracker, PapersConfig},
        rss::{RssConfig, RssTracker},
    },
    bootstrap::config::Config,
    domain::NotifyRoute,
    ingress::{
        cli::QqCredentials,
        feishu::{self, FeishuWebhookNotifier},
        qq::{QqNotifier, api::QqApiClient},
    },
    ports::{channel::NotifyChannel, storage::TaskStore},
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
    let categories = arxiv
        .categories
        .clone()
        .filter(|c| !c.is_empty())
        .context("sources.arxiv.categories required (e.g. [\"cs.CR\", \"cs.AI\"])")?;
    Ok(PaperTracker::new(
        store,
        ArxivClient::new(),
        PapersConfig {
            categories,
            keywords: arxiv.keywords.clone().unwrap_or_default(),
            interests: arxiv.interests.clone().unwrap_or_default(),
            max_results: arxiv.max_results.unwrap_or(100),
            interval_secs: arxiv.interval_secs.unwrap_or(21_600),
            backend: arxiv.backend.clone().unwrap_or_else(|| "codex".into()),
            notify: source_notify(config, arxiv.notify)?,
        },
    ))
}

/// 由 [sources.rss] 配置组装追踪器；通知路由同样来自全局 [notify] 出口。
pub fn rss_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<RssTracker> {
    let rss = &config.sources.rss;
    let feeds = rss
        .feeds
        .clone()
        .filter(|f| !f.is_empty())
        .context("sources.rss.feeds required")?;
    Ok(RssTracker::new(
        store,
        FeedClient::new(),
        RssConfig {
            feeds,
            keywords: rss.keywords.clone().unwrap_or_default(),
            interests: rss.interests.clone().unwrap_or_default(),
            max_items_per_feed: rss.max_items_per_feed.unwrap_or(50),
            interval_secs: rss.interval_secs.unwrap_or(21_600),
            backend: rss.backend.clone().unwrap_or_else(|| "codex".into()),
            notify: source_notify(config, rss.notify)?,
        },
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
    let token = std::env::var("RU41_ECARD_TOKEN")
        .ok()
        .or_else(|| ecard.token.clone())
        .filter(|t| !t.trim().is_empty())
        .context("ecard token required: RU41_ECARD_TOKEN or sources.ecard.token")?;
    let room = EcardRoom {
        impl_type: ecard
            .impl_type
            .clone()
            .context("sources.ecard.impl_type required")?,
        school_area_no: ecard
            .school_area_no
            .clone()
            .context("sources.ecard.school_area_no required")?,
        building_no: ecard
            .building_no
            .clone()
            .context("sources.ecard.building_no required")?,
        room_num: ecard
            .room_num
            .clone()
            .context("sources.ecard.room_num required")?,
    };
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
        EcardConfig {
            room,
            token,
            power_threshold: ecard.power_threshold.unwrap_or(10.0),
            water_threshold: ecard.water_threshold.unwrap_or(1.0),
            interval_secs: ecard.interval_secs.unwrap_or(86_400),
            notify_target,
        },
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
