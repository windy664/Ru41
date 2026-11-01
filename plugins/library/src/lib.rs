pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::LibrarySection;
pub use qq_command::LibraryQqCommand;
pub use service::{LibraryConfig, LibraryPollReport, LibraryTracker};
pub use source::{HoldingItem, LibraryWatch, LoanPage, LoanRecord, LibraryClient, SearchBook, SearchPage};

/// 注册表定义：library 的调度清单、tracker、QQ 命令闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "library",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("library"),
    schedule: |config| {
        let library = config.sources.get_or_default::<crate::LibrarySection>("library");
        vec![ru41::modules::JobSchedule::simple(
            "library",
            library
                .interval_secs
                .unwrap_or(config::LibrarySection::DEFAULT_INTERVAL_SECS),
            library.is_ready(),
            (!library.is_ready())
                .then(|| "需 sources.library 或 RU41_JWXT_USERNAME/PASSWORD（与教务同套 CAS 凭证）".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "library" {
            return Ok(None);
        }
        let library = config.sources.get::<crate::LibrarySection>("library")?;
        let (notifier, notify_target) =
            ru41::bootstrap::assembly::notify_pair(config, library.notify)?;
        let service_config = library.resolved(notify_target)?;
        let client = LibraryClient::new(
            service_config.username.clone(),
            service_config.password.clone(),
            service_config.cas_base.clone(),
            service_config.cas_modulus.clone(),
            service_config.cas_exponent.clone(),
            service_config.unified_base.clone(),
            service_config.find_base.clone(),
            service_config.mobile_base.clone(),
            service_config.group_code.clone(),
            service_config.session_path.clone(),
        );
        Ok(Some(Box::new(LibraryTracker::new(
            store,
            client,
            notifier,
            service_config,
        ))))
    },
    qq_handlers: |config, _| {
        let library = config.sources.get::<crate::LibrarySection>("library")?;
        let service_config = library.resolved(String::new())?;
        Ok(vec![std::sync::Arc::new(LibraryQqCommand::new(
            std::sync::Arc::new(LibraryClient::new(
                service_config.username,
                service_config.password,
                service_config.cas_base,
                service_config.cas_modulus,
                service_config.cas_exponent,
                service_config.unified_base,
                service_config.find_base,
                service_config.mobile_base,
                service_config.group_code,
                service_config.session_path,
            )),
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
    ru41::plugins::PluginEntry::new("library", || DEF)
}
