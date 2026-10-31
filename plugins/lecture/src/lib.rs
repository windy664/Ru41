//! 课堂录音附属：QQ 单聊收录音 → ffmpeg 切片 → ASR 转写（临时产物）
//! → 转写内联进 agent prompt → 结构化课堂笔记写 Vault（唯一沉淀）。
//! handler 只负责即时下载与排队（QQ CDN 链接有时效），重活在 worker 轮询里做。

pub mod asr;
pub mod config;
pub mod handler;
pub mod pending;
pub mod service;


pub use config::{LectureConfig, LectureSection};
pub use handler::LectureHandler;
pub use service::LectureWorker;

use ru41::modules::{JobSchedule, ModuleDef, DEFAULT_PRIORITY, no_cli, no_image_consumers};

/// 注册表定义：lecture 一个 worker job + 一个 QQ 自然语言拦截。
pub static DEF: ModuleDef = ModuleDef {
    name: "lecture",
    priority: DEFAULT_PRIORITY,
    config_key: Some("lecture"),
    schedule: |config| {
        let lecture = config.sources.get_or_default::<LectureSection>("lecture");
        let ready = lecture.is_ready();
        vec![JobSchedule::simple(
            "lecture",
            LectureSection::DEFAULT_INTERVAL_SECS,
            ready,
            lecture.not_ready_reason(),
        )]
    },
    tracker: |config, store, name| {
        if name != "lecture" {
            return Ok(None);
        }
        let lecture = config.sources.get::<LectureSection>("lecture")?;
        anyhow::ensure!(
            lecture.is_ready(),
            "sources.lecture requires ASR api_key"
        );
        Ok(Some(Box::new(LectureWorker::new(
            store,
            config.todo.options()?,
            lecture.resolved()?,
        ))))
    },
    qq_handlers: |config, _| {
        let lecture = config.sources.get_or_default::<LectureSection>("lecture");
        if !lecture.is_ready() {
            return Ok(Vec::new());
        }
        Ok(vec![std::sync::Arc::new(LectureHandler::new(
            lecture.resolved()?,
        )) as std::sync::Arc<dyn ru41::ports::commands::CommandHandler>])
    },
    image_consumers: no_image_consumers,
    cli: no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("lecture", || DEF)
}
