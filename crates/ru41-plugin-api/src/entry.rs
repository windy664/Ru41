//! 插件入口元信息与握手常量（边界两侧共用同一定义）。
//!
//! ## 握手硬门（宿主加载时校验，任一不匹配拒绝加载）
//!
//! 1. `magic`：防把无关 .so 当 ru41 插件。
//! 2. `api_version`：本模块内手工维护的结构版本，PluginEntry 自身布局或
//!    加载协议变更时 +1。
//! 3. `api_crate_version`：ru41-plugin-api crate 的 semver；边界闭包内任何
//!    公开类型/trait/签名变更都必须 bump 它。**宿主业务代码（app/adapters/
//!    ingress/main）变更不影响本版本，旧插件继续加载，无需重编。**
//! 4. `deps_stamp`：tokio/serde 等 ABI 相关外部依赖在 Cargo.lock 中解析
//!    版本的摘要（本 crate build.rs 生成）。宿主与插件解析版本不同即拒。
//!
//! `git_hash` 仅诊断：不一致记日志，不再拒绝加载。

use crate::module::ModuleDef;

/// 插件入口符号名。
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"ru41_plugin_entry";
/// ABI 魔数：防把无关 .so 当 ru41 插件。
pub const PLUGIN_API_MAGIC: u64 = 0x7275_3431_5f70_6c75;
/// 入口结构版本：PluginEntry 布局或握手协议变更时 +1。
/// v2：ru41_version/pkg 版本门退役，改为 api crate 版本 + deps stamp。
pub const PLUGIN_API_VERSION: u32 = 2;
/// 本 crate 版本（握手硬门之一）。
pub const API_CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 外部依赖 stamp（握手硬门之一；本 crate build.rs 从 Cargo.lock 生成）。
pub const DEPS_STAMP: &str = env!("RU41_DEPS_STAMP");

/// 插件入口返回的元信息。`&'static str` 等 Rust 原生布局依赖
/// "同一 rustc + 同一 api crate 版本"前提，握手失败即拒绝加载。
pub struct PluginEntry {
    pub magic: u64,
    pub api_version: u32,
    pub api_crate_version: &'static str,
    pub deps_stamp: &'static str,
    /// 编译插件时仓库的 git commit；仅诊断，不参与加载决策。
    pub git_hash: &'static str,
    pub name: &'static str,
    pub register: fn() -> ModuleDef,
}

impl PluginEntry {
    /// 插件侧构造入口：版本字段自动取编译时环境。
    pub fn new(name: &'static str, register: fn() -> ModuleDef) -> Self {
        Self {
            magic: PLUGIN_API_MAGIC,
            api_version: PLUGIN_API_VERSION,
            api_crate_version: API_CRATE_VERSION,
            deps_stamp: DEPS_STAMP,
            git_hash: env!("RU41_GIT_HASH"),
            name,
            register,
        }
    }
}
