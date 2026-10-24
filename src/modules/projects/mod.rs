pub mod config;
pub mod service;

pub use config::ProjectsSection;
pub use service::{ProjectsConfig, ProjectsSyncReport, ProjectsTracker};

/// 注册表定义：projects 的调度清单、tracker 构造都在本模块内闭环。
use anyhow::Context as _;

pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "projects",
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule {
            name: "projects",
            interval_secs: config
                .sources
                .projects
                .interval_secs
                .unwrap_or(config::ProjectsSection::DEFAULT_INTERVAL_SECS),
            enabled: ready,
            disabled_reason: (!ready)
                .then(|| "需 [obsidian] vault 已配置（项目库写在 vault 里）".into()),
        }]
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
            config.sources.projects.resolved(vault_root)?,
            vault,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
