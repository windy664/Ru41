//! 动态插件（Pumpkin/bevy 式 cdylib）：宿主启动时扫描插件目录，
//! libloading 加载 `.so`、握手、注册外部 ModuleDef 并入统一调度。
//!
//! 边界类型（PluginEntry/握手常量/bridge）定义在 ru41-plugin-api，
//! 本文件持有宿主侧加载机器（libloading 只在宿主链接）。
//!
//! ## 插件 ABI
//!
//! 插件是 `crate-type = ["cdylib"]` 的独立 crate，静态链接 ru41 lib（其
//! re-export ru41-plugin-api 的边界类型），并导出一个入口符号：
//!
//! ```ignore
//! #[unsafe(no_mangle)]
//! pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
//!     ru41::plugins::PluginEntry::new("my-module", || DEF)
//! }
//! ```
//!
//! ## 握手硬门（任一不匹配拒绝加载）
//!
//! 1. magic + 入口结构版本；
//! 2. **ru41-plugin-api crate 版本完全一致**——边界闭包（ModuleDef/ports/
//!    domain/Config）全部住在该 crate，版本一致即布局一致；
//! 3. **deps stamp 一致**——tokio/serde 等 ABI 相关外部依赖在 Cargo.lock
//!    的解析版本摘要（api crate build.rs 生成；插件 lockfile 由部署脚本
//!    从仓库根同步，保证双方解析到同一组版本）。
//!
//! 宿主业务代码（app/adapters/ingress/main）变更不再触发插件重编与拒载；
//! git commit 仅作诊断日志。仍需同一 rustc（Rust 无稳定 ABI）。
//!
//! ## 约束（与 Pumpkin 同款，不可协商）
//!
//! - **无进程隔离**：插件 panic 可带崩宿主进程（cdylib 本质是同进程），
//!   爆炸半径靠容器模型兜底（每模块独立容器）；async 入口过 bridge 后
//!   panic 降级为 Err。
//! - **永不卸载**：Library 加载后 mem::forget，运行中不 dlclose
//!   （卸载持有运行中任务的代码是 UB）。热更新 = 重启对应容器。
//! - 插件不用 linkme 自注册（dylib 有独立 section，宿主 slice 看不到），
//!   一律走入口函数显式返回 DEF。

use crate::modules::ModuleDef;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use ru41_plugin_api::bridge::bridge;
pub use ru41_plugin_api::entry::{
    API_CRATE_VERSION, DEPS_STAMP, PLUGIN_API_MAGIC, PLUGIN_API_VERSION, PLUGIN_ENTRY_SYMBOL,
    PluginEntry,
};

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
    // Safety：插件与宿主同一 rustc + 同一 ru41-plugin-api 版本（下方握手校验），
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
        "入口结构版本不匹配：插件 v{} / 宿主 v{}",
        meta.api_version,
        PLUGIN_API_VERSION
    );
    anyhow::ensure!(
        meta.api_crate_version == API_CRATE_VERSION,
        "ru41-plugin-api 版本不匹配：插件 {} / 宿主 {}（边界类型已变更，需重编插件）",
        meta.api_crate_version,
        API_CRATE_VERSION
    );
    anyhow::ensure!(
        meta.deps_stamp == DEPS_STAMP,
        "依赖版本不匹配：插件 {} / 宿主 {}（tokio/serde 等解析版本漂移，需同步 lockfile 重编）",
        meta.deps_stamp,
        DEPS_STAMP
    );
    let host_hash = env!("RU41_GIT_HASH");
    if meta.git_hash != host_hash {
        // 业务代码差异不影响 ABI（边界由 api crate 版本把守），只记诊断日志。
        eprintln!(
            "[plugins] 提示：{} 与宿主 git commit 不同（插件 {} / 宿主 {}），api 版本一致按放行处理",
            meta.name, meta.git_hash, host_hash
        );
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
