//! 跨平台作业聚合：学习通与智慧树的作业在 QQ 侧是同一件事（`/作业`），
//! 与 Obsidian 里的「作业总表.md」同一聚合视角。新作业平台接入时在
//! `collect_rows` 加一行映射即可。

pub mod qq_command;

pub use qq_command::HomeworkQqCommand;
