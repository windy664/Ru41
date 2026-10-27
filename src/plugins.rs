//! 动态插件（Pumpkin/bevy 式 cdylib）：宿主启动时扫描插件目录，
//! libloading 加载 `.so`、握手 ABI 版本、注册外部 ModuleDef 并入统一调度。
//!
//! ## 插件 ABI（插件与宿主必须同一 rustc + 同一 ru41 版本编译）
//!
//! 插件是 `crate-type = ["cdylib"]` 的独立 crate，静态链接**同一 git 版本**的
//! ru41 lib，并导出一个入口符号：
//!
//! ```ignore
//! #[unsafe(no_mangle)]
//! pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
//!     ru41::plugins::PluginEntry::new::<MyModule>()
//! }
//! ```
//!
//! ## 约束（与 Pumpkin 同款，不可协商）
//!
//! - **版本锁**：rustc、ru41 crate 版本、依赖锁三者必须一致，否则
//!   类型布局/vtable 可能漂移（UB 而非报错）。握手查 magic + API 版本 +
//!   ru41 版本，不匹配拒绝加载。
//! - **无进程隔离**：插件 panic 可带崩宿主进程（cdylib 本质是同进程）。
//!   爆炸半径靠容器模型兜底（每模块独立容器）。
//! - **永不卸载**：Library 加载后 mem::forget，运行中不 dlclose
//!   （卸载持有运行中任务的代码是 UB）。热更新 = 重启对应容器。
//! - 插件不用 linkme 自注册（dylib 有独立 section，宿主 slice 看不到），
//!   一律走入口函数显式返回 DEF。

use crate::modules::ModuleDef;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 插件入口符号名。
pub const PLUGIN_ENTRY_SYMBOL: &[u8] = b"ru41_plugin_entry";
/// ABI 魔数：防把无关 .so 当 ru41 插件。
pub const PLUGIN_API_MAGIC: u64 = 0x7275_3431_5f70_6c75;
/// ABI 版本：ModuleDef/trait 签名/关键依赖（tokio/serde）破坏性变更时 +1。
pub const PLUGIN_API_VERSION: u32 = 1;

/// 插件入口返回的元信息。`&'static str` 等 Rust 原生布局依赖
/// "同一 rustc 编译"前提，握手失败即拒绝加载，不做跨编译器兼容。
pub struct PluginEntry {
    pub magic: u64,
    pub api_version: u32,
    pub ru41_version: &'static str,
    /// 编译插件时 ru41 crate 的 git commit；pkg 版本不变时布局仍可能漂移，
    /// 双方都有 git 元数据时要求完全一致，任一方 unknown 降级为告警放行。
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
            ru41_version: env!("CARGO_PKG_VERSION"),
            git_hash: env!("RU41_GIT_HASH"),
            name,
            register,
        }
    }
}

/// 已加载插件：Library 句柄必须活得和 defs 一样久（进程生命周期）。
struct LoadedPlugins {
    #[allow(dead_code)] // 故意持有：dlclose 是 UB 来源
    libs: Vec<libloading::Library>,
    defs: Vec<&'static ModuleDef>,
}

static LOADED: OnceLock<LoadedPlugins> = OnceLock::new();

/// 已加载插件的 DEF 列表；未初始化（未跑 load_all）视为空。
pub fn external_defs() -> &'static [&'static ModuleDef] {
    LOADED.get().map(|l| l.defs.as_slice()).unwrap_or(&[])
}

/// 扫描目录加载全部 `.so` 插件；单个失败记日志跳过，不挡其他插件与宿主启动。
/// 返回成功加载数量。
pub fn load_all(dir: &Path) -> Result<usize> {
    let mut libs = Vec::new();
    let mut defs: Vec<&'static ModuleDef> = Vec::new();
    let mut so_files: Vec<PathBuf> = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "so") {
                so_files.push(path);
            }
        }
        so_files.sort();
    }
    for path in so_files {
        match load_one(&path) {
            Ok((lib, def)) => {
                eprintln!("[plugins] 已加载 {}（{}）", def.name, path.display());
                libs.push(lib);
                defs.push(def);
            }
            Err(error) => eprintln!("[plugins] 跳过 {}: {error:#}", path.display()),
        }
    }
    let count = defs.len();
    LOADED
        .set(LoadedPlugins { libs, defs })
        .map_err(|_| anyhow::anyhow!("plugins::load_all 只能调用一次"))?;
    Ok(count)
}

fn load_one(path: &Path) -> Result<(libloading::Library, &'static ModuleDef)> {
    // Safety：插件与宿主同一 rustc/ru41 版本（入口握手校验），
    // 且 Library 永不卸载（调用方 mem::forget 语义持有）。
    let lib = unsafe { libloading::Library::new(path) }
        .with_context(|| format!("dlopen {}", path.display()))?;
    let entry: libloading::Symbol<unsafe extern "C" fn() -> PluginEntry> =
        unsafe { lib.get(PLUGIN_ENTRY_SYMBOL) }.context("缺少 ru41_plugin_entry 导出符号")?;
    let meta = unsafe { entry() };
    anyhow::ensure!(
        meta.magic == PLUGIN_API_MAGIC,
        "magic 不匹配（不是 ru41 插件或 ABI 损坏）"
    );
    anyhow::ensure!(
        meta.api_version == PLUGIN_API_VERSION,
        "ABI 版本不匹配：插件 v{} / 宿主 v{}",
        meta.api_version,
        PLUGIN_API_VERSION
    );
    anyhow::ensure!(
        meta.ru41_version == env!("CARGO_PKG_VERSION"),
        "ru41 版本不匹配：插件 {} / 宿主 {}（插件需用同一版本 ru41 crate 重编）",
        meta.ru41_version,
        env!("CARGO_PKG_VERSION")
    );
    let host_hash = env!("RU41_GIT_HASH");
    if meta.git_hash != host_hash {
        // 双方都有 hash 且不同：布局可能已漂移，硬拒。
        // 插件 unknown 而宿主有 hash：插件没按规范编（缺 OVERRIDE），硬拒。
        // 仅宿主 unknown（无 git 元数据的非常规构建）才降级告警放行。
        if host_hash == "unknown" {
            eprintln!(
                "[plugins] 警告：宿主无 git 元数据，无法校验 {}（插件 {}），按版本号放行",
                meta.name, meta.git_hash
            );
        } else if meta.git_hash == "unknown" {
            anyhow::bail!(
                "插件 {} 未嵌入 git commit（需用 RU41_GIT_HASH_OVERRIDE 重编）",
                meta.name
            );
        } else {
            anyhow::bail!(
                "git commit 不匹配：插件 {} / 宿主 {}（pkg 版本相同但布局可能已漂移，需重编插件）",
                meta.git_hash,
                host_hash
            );
        }
    }
    // DEF 泄漏为 'static：与宿主内建模块同生命周期，永不回收（不卸载原则）。
    let def: &'static ModuleDef = Box::leak(Box::new((meta.register)()));
    Ok((lib, def))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_dir_loads_zero() {
        let dir = std::env::temp_dir().join("ru41-no-such-plugin-dir");
        let loaded = load_all(&dir).unwrap();
        assert_eq!(loaded, 0);
        assert!(external_defs().is_empty());
    }
}
