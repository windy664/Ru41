//! 通义听悟（Aliyun Tingwu）附属：把音视频变成逐字稿。
//!
//! ## 为什么走网页版内部 API
//!
//! 官方 OpenAPI（`/openapi/v1/trans/submit`）要 `Authorization: Bearer <transToken>`，
//! 而 transToken 的来源至今没在 umi.js 里找到（只见过分享场景在用）；网页版用
//! **cookie** 认证，能直接用用户自己的账号额度。所以本插件走网页版内部 API。
//!
//! ## 凭据怎么管
//!
//! cookie 存 ru41 的**凭据托管库**（`ru41 secret set tingwu-cookie '<cookie>'`），
//! 插件按名字取，**真值不进仓库、不进模型上下文**。配置里只写凭据叫什么。
//!
//! ## 链路
//!
//! 见 [`client`] 模块的注释：generatePutLink → PUT 上传 → syncPutLink →
//! 轮询 getTransStatus → getTransResult。参数逐字段对齐网页版（少一个就 DefaultError）。
//!
//! 输出落在 `output_dir`（默认 state.db 同级的 `tingwu/`，容器内 `/data/tingwu`）。

pub mod cli;
pub mod client;
pub mod config;
pub mod transcriber;

pub use cli::{MODULE_NAME, tingwu_cli};
pub use client::{TingwuClient, Transcript};
pub use config::TingwuSection;
pub use transcriber::TingwuTranscriber;

/// 凭据托管库里存听悟 cookie 的那条凭据的名字（默认）。
pub const CREDENTIAL_LABEL: &str = "tingwu-cookie";

/// 注册表定义：只有专属 CLI（转写是用户触发的，不排周期任务）。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: MODULE_NAME,
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some(MODULE_NAME),
    schedule: |_| vec![],
    tracker: |_, _, _| Ok(None),
    // 不接 QQ 交互命令：音视频文件从聊天消息落地到附件目录，路径由 agent 决定，
    // 交互命令拿不到可靠路径 —— 走 CLI + 技能包。
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: tingwu_cli,
};

/// 宿主加载时注册转写能力（听悟优先 → 本地兜底）。
///
/// 每个 ru41 容器都会加载全部插件，所以每个进程的注册表里都有这一项；
/// `lecture` 的 worker 转写前查表就能用上听悟。
fn register_def() -> ru41::modules::ModuleDef {
    ru41::ports::transcriber::register_transcriber(std::sync::Arc::new(
        TingwuTranscriber::default(),
    ));
    DEF
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new(MODULE_NAME, register_def)
}
