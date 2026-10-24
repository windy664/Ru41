pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::EcardSection;
pub use qq_command::EcardQqCommand;
pub use service::{EcardConfig, EcardReport, EcardTracker};
pub use source::{EcardBalance, EcardClient, EcardRoom};

/// 注册表定义：ecard 的调度清单、tracker 构造、QQ 命令都在本模块内闭环。
use anyhow::Context as _;

pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "ecard",
    schedule: |config| {
        let ecard = &config.sources.ecard;
        vec![super::JobSchedule::simple(
            "ecard",
            ecard
                .interval_secs
                .unwrap_or(config::EcardSection::DEFAULT_INTERVAL_SECS),
            ecard.is_ready(),
            (!ecard.is_ready())
                .then(|| "需 sources.ecard 房间信息及 token（或 RU41_ECARD_TOKEN）".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "ecard" {
            return Ok(None);
        }
        let ecard = &config.sources.ecard;
        let base_url = ecard
            .base_url
            .clone()
            .context("sources.ecard.base_url required")?;
        let (notifier, notify_target) =
            crate::bootstrap::assembly::notify_pair(config, ecard.notify)?;
        Ok(Some(Box::new(EcardTracker::new(
            store,
            EcardClient::new(base_url),
            notifier,
            ecard.resolved(notify_target)?,
        ))))
    },
    qq_handlers: |_, backend| {
        Ok(vec![std::sync::Arc::new(EcardQqCommand::new(backend.to_owned()))])
    },
};
