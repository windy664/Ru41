//! 跨平台作业聚合：学习通与智慧树的作业在 QQ 侧是同一件事（`/作业`），
//! 与 Obsidian 里的「作业总表.md」同一聚合视角。新作业平台接入时在
//! `collect_rows` 加一行映射即可。

pub mod qq_command;
pub mod work_common;

pub use qq_command::HomeworkQqCommand;

// 微助教作业状态判定走本共享层，chaoxing 的作业总表不再直连 teachermate。
pub use crate::modules::teachermate::homework_status_open;

/// 注册表定义：跨平台 /作业 聚合命令。注意 MODULES 里的注册顺序——
/// 本模块必须在 zhihuishu 之前（first-match-wins，/作业 先于平台专属命令命中）。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "homework",
    schedule: |_| Vec::new(),
    tracker: |_, _, _| Ok(None),
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(HomeworkQqCommand::new(
            config
                .sources
                .zhihuishu
                .resolved(String::new())?
                .report_path,
        ))])
    },
};
