//! 竞赛日程总览（外部插件）：从内建 src/modules/competitions 迁出，
//! 验证"带配置 + vault + 多源抓取"的复杂模块在 cdylib 形态下工作。

mod filter;
mod overview;
mod service;

pub use service::{CompetitionsService, DEF, OVERVIEW_PATH};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("competitions", || DEF)
}
