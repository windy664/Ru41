//! 页面监控附属：盯没有 RSS 的机会源头页面（赛事官网/公告页），内容变更即通知。

mod config;
mod service;

pub use config::PagewatchSection;
use ru41::app::obsidian::ObsidianSectionExt;
pub use service::{PagewatchConfig, PagewatchReport, PagewatchTracker, WatchedPage};

/// 注册表定义：pagewatch 轮询 job。无 QQ 命令（只看不回话）。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "pagewatch",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("pagewatch"),
    schedule: |config| {
        let section = config
            .sources
            .get_or_default::<crate::PagewatchSection>("pagewatch");
        vec![ru41::modules::JobSchedule::simple(
            "pagewatch",
            section
                .interval_secs
                .unwrap_or(config::PagewatchSection::DEFAULT_INTERVAL_SECS),
            section.is_ready(),
            (!section.is_ready()).then(|| "需 sources.pagewatch.pages（至少一个监控页面）".into()),
        )]
    },
    tracker: |config, store, name| {
        if name != "pagewatch" {
            return Ok(None);
        }
        let section = config.sources.get::<crate::PagewatchSection>("pagewatch")?;
        let route = ru41::bootstrap::assembly::source_notify(config, section.notify)?;
        let notifier = match &route {
            Some(route) => Some(ru41::bootstrap::assembly::default_notifier(
                config,
                &route.channel,
            )?),
            None => None,
        };
        // 变更记录落 vault（Obsidian 里看，不打扰 QQ）；obsidian 未配置则只走通知。
        let vault = if config.obsidian.is_ready() {
            let resolved = config.obsidian.resolved(None)?;
            Some(ru41::bootstrap::assembly::vault_client(&resolved))
        } else {
            None
        };
        Ok(Some(Box::new(
            PagewatchTracker::new(store, notifier, section.resolved(route)?).with_vault_opt(vault),
        )))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("pagewatch", || DEF)
}
