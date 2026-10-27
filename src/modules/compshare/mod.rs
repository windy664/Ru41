pub mod config;
pub mod service;
pub mod source;

pub use config::CompshareSection;
pub use service::{CompshareConfig, CompshareReport, CompshareTracker};
pub use source::{CapacitySpec, CompshareClient, StockSnapshot, StockState};

/// 注册表定义：compshare 的调度清单与 tracker 构造在本模块内闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "compshare",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("compshare"),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<CompshareSection>("compshare");
        vec![super::JobSchedule::simple(
            "compshare",
            section
                .interval_secs
                .unwrap_or(config::CompshareSection::DEFAULT_INTERVAL_SECS),
            section.is_ready(),
            (!section.is_ready())
                .then(|| "需 sources.compshare.project_id 及 RU41_COMPSHARE_COOKIE".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "compshare" {
            return Ok(None);
        }
        let section = config.sources.get::<CompshareSection>("compshare")?;
        let (notifier, notify_target) =
            crate::bootstrap::assembly::notify_pair(config, section.notify)?;
        let resolved = section.resolved(notify_target)?;
        let base_url = resolved.base_url.clone();
        Ok(Some(Box::new(CompshareTracker::new(
            store,
            CompshareClient::new(base_url),
            notifier,
            resolved,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
