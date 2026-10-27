//! 附属注册表：新模块只需①写自己目录 ②这里加一行——调度清单、
//! tracker 装配、QQ 命令注册全部数据驱动，不再碰 main.rs/assembly.rs。
//!
//! ModuleDef/JobSchedule 等边界类型定义在 ru41-plugin-api（宿主与插件
//! 同一份），本文件保持原路径并持有宿主侧注册表。

pub mod chaoxing;
pub mod jwxt;
pub mod obsidian;
pub mod planner;
pub mod teachermate;
pub mod youtube;

pub use ru41_plugin_api::module::*;

/// 分布式注册表：各模块在自己的 mod.rs 里用
/// `#[linkme::distributed_slice(MODULES)] pub static DEF: ModuleDef = ...`
/// 自注册，新增附属不再编辑本文件的注册清单。
/// 注意：linkme 的遍历顺序不保证稳定，消费方一律走 `sorted()`。
#[linkme::distributed_slice]
pub static MODULES: [ModuleDef];

/// 按 (priority, name) 稳定排序后的注册表视图。
pub fn sorted() -> Vec<&'static ModuleDef> {
    let mut defs: Vec<&'static ModuleDef> = MODULES.iter().collect();
    defs.extend(crate::plugins::external_defs());
    defs.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.name.cmp(b.name)));
    defs
}
