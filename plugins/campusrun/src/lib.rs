//! 校园跑附属：只读运动世界校园（pace-forge 控制台）的校园跑进度与记录。
//!
//! 数据源是生产机上已部署的 pace-forge 控制台（`http://<host>:9999`）。
//! 本插件**只读**：学期进度、跑步记录、违规状态；不提交任何跑步记录。
//!
//! 文档：docs/sources-campusrun.md

pub mod cli;
pub mod client;
pub mod config;
pub mod model;
pub mod qq_command;
pub mod render;
pub mod review;
pub mod service;
pub mod submit;

pub use cli::{MODULE_NAME, campusrun_cli};
pub use client::{CampusrunClient, CampusrunError};
pub use config::CampusrunSection;
pub use qq_command::CampusrunQqCommand;
pub use service::CampusrunTracker;
pub use submit::{PendingSubmit, RunIntent, RunParams};

/// 注册表定义：调度清单、只读 tracker、QQ 命令、专属 CLI。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: MODULE_NAME,
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some(MODULE_NAME),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<CampusrunSection>(MODULE_NAME);
        let ready = section.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            MODULE_NAME,
            3600,
            ready,
            (!ready).then(|| {
                "需 pace-forge 控制台令牌（环境变量 PACE_FORGE_TOKEN，或 [sources.campusrun].token）"
                    .into()
            }),
        )]
    },
    tracker: |config, store, name| {
        if name != MODULE_NAME {
            return Ok(None);
        }
        let section = config.sources.get::<CampusrunSection>(MODULE_NAME)?;
        let client = CampusrunClient::new(&section).map_err(|error| anyhow::anyhow!("{error}"))?;
        // 复查需要凭据托管库（读上次提交的 rrid）与通知通道（判违规了主动告诉用户）。
        let (notifier, target) = ru41::bootstrap::assembly::notify_pair(config, Some(true))?;
        Ok(Some(Box::new(CampusrunTracker::with_review(
            client,
            store,
            notifier,
            target,
        ))))
    },
    qq_handlers: |config, _| {
        let section = config
            .sources
            .get_or_default::<CampusrunSection>(MODULE_NAME);
        // 通知通道：后台提交完成后把结果发回原会话（插件不直连渠道 API，
        // 走宿主装配的 NotifyChannel）。
        let (notifier, _target) = ru41::bootstrap::assembly::notify_pair(config, Some(true))?;
        Ok(vec![std::sync::Arc::new(CampusrunQqCommand::new(
            section, notifier,
        ))])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: campusrun_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new(MODULE_NAME, || DEF)
}
