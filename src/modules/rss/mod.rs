pub mod config;
pub mod service;

pub use config::RssSection;
pub use service::{RssConfig, RssPollReport, RssTracker};

/// 注册表定义：rss 的调度清单、tracker 构造都在本模块内闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "rss",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("rss"),
    schedule: |config| {
        let rss = config
            .sources
            .get_or_default::<crate::modules::rss::RssSection>("rss");
        let ready = rss.feeds.as_ref().is_some_and(|f| !f.is_empty());
        vec![super::JobSchedule::simple(
            "rss",
            rss.interval_secs
                .unwrap_or(config::RssSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "配置 sources.rss.feeds 后启用".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "rss" {
            return Ok(None);
        }
        let rss = config
            .sources
            .get::<crate::modules::rss::RssSection>("rss")?;
        let tracker = RssTracker::new(
            store,
            crate::adapters::sources::feed::FeedClient::new(),
            rss.resolved(crate::bootstrap::assembly::source_notify(
                config, rss.notify,
            )?)?,
        );
        Ok(Some(Box::new(tracker)))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
