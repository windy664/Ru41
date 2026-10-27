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

pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "ecard",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("ecard"),
    schedule: |config| {
        let ecard = config
            .sources
            .get_or_default::<crate::EcardSection>("ecard");
        vec![ru41::modules::JobSchedule::simple(
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
        let ecard = config
            .sources
            .get::<crate::EcardSection>("ecard")?;
        let base_url = ecard
            .base_url
            .clone()
            .context("sources.ecard.base_url required")?;
        let (notifier, notify_target) =
            ru41::bootstrap::assembly::notify_pair(config, ecard.notify)?;
        Ok(Some(Box::new(EcardTracker::new(
            store,
            EcardClient::new(base_url),
            notifier,
            ecard.resolved(notify_target)?,
        ))))
    },
    qq_handlers: |_, backend| {
        Ok(vec![std::sync::Arc::new(EcardQqCommand::new(
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
    ru41::plugins::PluginEntry::new("ecard", || DEF)
}
