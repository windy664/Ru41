pub mod config;
pub mod service;

pub use config::ProjectsSection;
pub use service::{ProjectsConfig, ProjectsSyncReport, ProjectsTracker};

/// 注册表定义：projects 的调度清单、tracker 构造都在本模块内闭环。
use anyhow::Context as _;

pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "projects",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("projects"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "projects",
            config
                .sources
                .get_or_default::<config::ProjectsSection>("projects")
                .interval_secs
                .unwrap_or(config::ProjectsSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "需 [obsidian] vault 已配置（项目库写在 vault 里）".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "projects" {
            return Ok(None);
        }
        let _ = store;
        let vault = ru41::bootstrap::assembly::vault_if_ready(config, true)?
            .context("projects 需要 [obsidian] vault 已配置（项目库写在 vault 里）")?;
        let vault_root = config.obsidian.resolved(None)?.vault_path;
        Ok(Some(Box::new(ProjectsTracker::new(
            config
                .sources
                .get::<crate::ProjectsSection>("projects")?
                .resolved(vault_root)?,
            vault,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("projects", || DEF)
}
