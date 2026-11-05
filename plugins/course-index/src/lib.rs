pub mod config;
pub mod service;

pub use config::CourseIndexSection;
pub use service::{
    CourseEntry, CourseIndexConfig, CourseIndexReport, CourseIndexTracker, PlatformWork,
    TimetableMeeting, WorkItem, render_course_index, render_overview,
};

use anyhow::Context as _;

/// 注册表定义：课程索引的调度清单与 tracker 构造在本模块内闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "course-index",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("course-index"),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<config::CourseIndexSection>("course-index");
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "course-index",
            section
                .interval_secs
                .unwrap_or(config::CourseIndexSection::DEFAULT_INTERVAL_SECS),
            ready,
            (!ready).then(|| "需 [obsidian] vault 已配置（课程索引写在 vault 里）".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "course-index" {
            return Ok(None);
        }
        let vault = ru41::bootstrap::assembly::vault_if_ready(config, true)?
            .context("course-index 需要 [obsidian] vault 已配置（课程索引写在 vault 里）")?;
        let section = config
            .sources
            .get::<crate::CourseIndexSection>("course-index")?;
        Ok(Some(Box::new(CourseIndexTracker::new(
            store,
            vault,
            section.resolved()?,
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: course_index_cli,
};

/// 专属 CLI：`ru41 course-index poll|sync`。
fn course_index_cli(
    config: &ru41::bootstrap::config::Config,
    store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = anyhow::Result<Option<serde_json::Value>>> + Send>,
> {
    // CLI 钩子返回的 future 必须是 'static：配置与参数在 async 之外物化。
    let command = args.first().cloned();
    let prepared = (|| -> anyhow::Result<Option<CourseIndexTracker>> {
        let Some(command) = command else {
            return Ok(None);
        };
        if !matches!(command.as_str(), "poll" | "sync") {
            return Ok(None);
        }
        let vault = ru41::bootstrap::assembly::vault_if_ready(config, true)?
            .context("course-index 需要 [obsidian] vault 已配置")?;
        let section = config
            .sources
            .get::<crate::CourseIndexSection>("course-index")?;
        Ok(Some(CourseIndexTracker::new(
            store.clone(),
            vault,
            section.resolved()?,
        )))
    })();
    Box::pin(async move {
        let Some(tracker) = prepared? else {
            return Ok(None);
        };
        Ok(Some(serde_json::to_value(tracker.sync().await?)?))
    })
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("course-index", || DEF)
}
