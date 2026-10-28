pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::ArxivSection;
pub use qq_command::PapersQqCommand;
pub use service::{PaperTracker, PapersConfig, PollReport, deep_read_task};
pub use source::{ArxivClient, ArxivEntry};

/// 注册表定义：papers 的调度清单、tracker 构造、QQ 命令都在本模块内闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "papers",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("arxiv"),
    schedule: |config| {
        let arxiv = config
            .sources
            .get_or_default::<crate::ArxivSection>("arxiv");
        let ready = arxiv.categories.as_ref().is_some_and(|c| !c.is_empty());
        vec![ru41::modules::JobSchedule::simple(
            "papers",
            arxiv
                .interval_secs
                .unwrap_or(config::ArxivSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "配置 sources.arxiv.categories 后启用".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "papers" {
            return Ok(None);
        }
        let arxiv = config.sources.get::<crate::ArxivSection>("arxiv")?;
        Ok(Some(Box::new(PaperTracker::new(
            store,
            ArxivClient::new(),
            arxiv.resolved(ru41::bootstrap::assembly::source_notify(
                config,
                arxiv.notify,
            )?)?,
        ))))
    },
    qq_handlers: |config, backend| {
        Ok(vec![std::sync::Arc::new(PapersQqCommand::new(
            ArxivClient::new(),
            config
                .sources
                .get::<crate::ArxivSection>("arxiv")?
                .interests
                .clone()
                .unwrap_or_default(),
            backend.to_owned(),
        ))])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("papers", || DEF)
}
