//! 自动刷课附属：让 ru41 自己管「刷课」这件事。
//!
//! 分工（2026-11-24 定）：
//!   - **sidecar**（生产机上的 `ru41-sidecar` 常驻容器，带 Playwright + Xvfb）：
//!     真的去刷 —— 开浏览器、点视频、浏览文档，让平台自己的前端把「已完成」标记点亮。
//!     平台的标记是浏览器里的 JS 上报的且请求体加密，没真实浏览器点不亮，所以这段留在 sidecar。
//!   - **本插件**（ru41 侧）：什么时候刷、刷完汇报、发通知、对外提供 CLI。
//!
//! 为什么不是把刷课整个写进 ru41：ru41 的容器里没有浏览器、也没挂 docker.sock，
//! 在 Rust 里重写一套浏览逻辑又慢又脆（页面结构一变就坏）。让 ru41 当调度中枢、
//! sidecar 当执行器，两边各干各擅长的事。
//!
//! 端点与时区：见 `sidecar/zhihuishu-sidecar/scripts/serve.py`。

pub mod cli;
pub mod client;
pub mod config;
pub mod service;

pub use cli::{MODULE_NAME, course_brush_cli};
pub use client::{BrushError, CourseBrushClient, Status};
pub use config::CourseBrushSection;
pub use service::CourseBrushTracker;

/// 注册表定义：调度清单、tracker、专属 CLI。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: MODULE_NAME,
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some(MODULE_NAME),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<CourseBrushSection>(MODULE_NAME);
        vec![ru41::modules::JobSchedule::simple(
            MODULE_NAME,
            section.interval().as_secs(),
            section.is_ready(),
            (!section.is_ready()).then(|| "未配置 sidecar 基址".to_owned()),
        )]
    },
    tracker: |config, _store, name| {
        if name != MODULE_NAME {
            return Ok(None);
        }
        let section = config.sources.get::<CourseBrushSection>(MODULE_NAME)?;
        let client =
            CourseBrushClient::new(&section).map_err(|error| anyhow::anyhow!("{error}"))?;
        // 通知出口走宿主装配（插件不直连渠道 API）。
        let (notifier, target) = ru41::bootstrap::assembly::notify_pair(config, Some(true))?;
        Ok(Some(Box::new(CourseBrushTracker::new(
            client,
            notifier,
            target,
            section.max_minutes(),
            section.max_courses(),
            section.notify(),
        ))))
    },
    // 刷课进度不接 QQ 交互命令：用户问「刷到哪了」用 `ru41 course-brush status` 或看通知。
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: course_brush_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new(MODULE_NAME, || DEF)
}
