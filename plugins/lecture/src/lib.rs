//! 课堂录音附属：QQ 单聊收录音 → ffmpeg 切片 → ASR 转写 → Vault 存档
//! → agent 整理结构化课堂笔记（期末复习周 RAG 素材）。
//! handler 只负责即时下载与排队（QQ CDN 链接有时效），重活在 worker 轮询里做。

pub mod asr;
pub mod config;
pub mod handler;
pub mod pending;
pub mod service;

use ru41::app::obsidian::ObsidianSectionExt;

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
        let vault_ready = config.obsidian.is_ready();
        let ready = lecture.is_ready() && vault_ready;
        vec![JobSchedule::simple(
            "lecture",
            LectureSection::DEFAULT_INTERVAL_SECS,
            ready,
            lecture
                .not_ready_reason()
                .or_else(|| {
                    (!vault_ready)
                        .then(|| "依赖 obsidian.vault_path（转写与笔记存 Vault）".to_string())
                }),
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
        anyhow::ensure!(
            config.obsidian.is_ready(),
            "sources.lecture requires obsidian.vault_path"
        );
        let vault_cfg = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(LectureWorker::new(
            store,
            ru41::bootstrap::assembly::vault_client(&vault_cfg),
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
