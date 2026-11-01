//! 活动日程总览（外部插件）：茶思屋会议日历 → 筛选 → vault 活动总览 + 新活动推送。

mod filter;
mod overview;
pub mod service;
pub mod source;

pub use service::{DEF, EventsSection, EventsService, OVERVIEW_PATH};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("events", || DEF)
}
