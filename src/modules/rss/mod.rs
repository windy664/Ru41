pub mod config;
pub mod service;

pub use config::RssSection;
pub use service::{RssConfig, RssPollReport, RssTracker};

/// 注册表定义：rss 的调度清单、tracker 构造都在本模块内闭环。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "rss",
    schedule: |config| {
        let rss = &config.sources.rss;
        let ready = rss.feeds.as_ref().is_some_and(|f| !f.is_empty());
        vec![super::JobSchedule {
            name: "rss",
            interval_secs: rss
                .interval_secs
                .unwrap_or(config::RssSection::DEFAULT_INTERVAL_SECS),
            enabled: ready,
            disabled_reason: (!ready).then(|| "配置 sources.rss.feeds 后启用".into()),
        }]
    },
    tracker: |config, store, name| {
        if name != "rss" {
            return Ok(None);
        }
        let rss = &config.sources.rss;
        let mut tracker = RssTracker::new(
            store,
            crate::adapters::sources::feed::FeedClient::new(),
            rss.resolved(crate::bootstrap::assembly::source_notify(
                config,
                rss.notify,
            )?)?,
        );
        // 挂 Vault：digest 精选完成后归档进 05-Opportunities（机会雷达闭环）。
        if let Some(vault) = crate::bootstrap::assembly::vault_if_ready(config, true)? {
            tracker = tracker.with_vault(vault);
        }
        Ok(Some(Box::new(tracker)))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
