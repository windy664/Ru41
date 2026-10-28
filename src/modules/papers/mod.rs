pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::ArxivSection;
pub use qq_command::PapersQqCommand;
pub use service::{PaperTracker, PapersConfig, PollReport, deep_read_task};
pub use source::{ArxivClient, ArxivEntry};

/// 注册表定义：papers 的调度清单、tracker 构造、QQ 命令都在本模块内闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "papers",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("arxiv"),
    schedule: |config| {
        let arxiv = config
            .sources
            .get_or_default::<crate::modules::papers::ArxivSection>("arxiv");
        let ready = arxiv.categories.as_ref().is_some_and(|c| !c.is_empty());
        vec![super::JobSchedule::simple(
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
        let arxiv = config
            .sources
            .get::<crate::modules::papers::ArxivSection>("arxiv")?;
        Ok(Some(Box::new(PaperTracker::new(
            store,
            ArxivClient::new(),
            arxiv.resolved(crate::bootstrap::assembly::source_notify(
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
                .get::<crate::modules::papers::ArxivSection>("arxiv")?
                .interests
                .clone()
                .unwrap_or_default(),
            backend.to_owned(),
        ))])
    },
    qq_image_consumers: |_| Ok(Vec::new()),
};
