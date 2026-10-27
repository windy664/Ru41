pub mod config;
pub mod service;

pub use config::ProjectsSection;
pub use service::{ProjectsConfig, ProjectsSyncReport, ProjectsTracker};

/// 注册表定义：projects 的调度清单、tracker 构造都在本模块内闭环。
use anyhow::Context as _;

#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "projects",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("projects"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
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
        let vault = crate::bootstrap::assembly::vault_if_ready(config, true)?
            .context("projects 需要 [obsidian] vault 已配置（项目库写在 vault 里）")?;
        let vault_root = config.obsidian.resolved(None)?.vault_path;
        Ok(Some(Box::new(ProjectsTracker::new(
            config
                .sources
                .get::<crate::modules::projects::ProjectsSection>("projects")?
                .resolved(vault_root)?,
            vault,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
