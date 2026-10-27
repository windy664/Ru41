pub mod config;
pub mod service;

pub use config::RssSection;
pub use service::{RssConfig, RssPollReport, RssTracker};

/// 注册表定义：rss 的调度清单、tracker 构造都在本模块内闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "rss",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("rss"),
    schedule: |config| {
        let rss = config
            .sources
            .get_or_default::<crate::RssSection>("rss");
        let ready = rss.feeds.as_ref().is_some_and(|f| !f.is_empty());
        vec![ru41::modules::JobSchedule::simple(
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
            .get::<crate::RssSection>("rss")?;
        let tracker = RssTracker::new(
            store,
            ru41::adapters::sources::feed::FeedClient::new(),
            rss.resolved(ru41::bootstrap::assembly::source_notify(
                config, rss.notify,
            )?)?,
        );
        Ok(Some(Box::new(tracker)))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("rss", || DEF)
}
