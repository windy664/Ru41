//! 跨平台作业聚合：学习通与智慧树的作业在 QQ 侧是同一件事（`/作业`），
//! 与 Obsidian 里的「作业总表.md」同一聚合视角。新作业平台接入时在
//! `collect_rows` 加一行映射即可。

pub mod qq_command;

pub use qq_command::HomeworkQqCommand;

// 微助教作业状态判定走本共享层，chaoxing 的作业总表不再直连 teachermate。
pub use ru41::app::homework_work::homework_status_open;

/// 注册表定义：跨平台 /作业 聚合命令。priority 10 保证注册顺序在
/// zhihuishu（20）之前——first-match-wins，/作业 先于平台专属命令命中。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "homework",
    priority: 10,
    config_key: None,
    schedule: |_| Vec::new(),
    tracker: |_, _, _| Ok(None),
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(HomeworkQqCommand::new(
            config
                .sources
                .get::<ru41_plugin_zhihuishu::ZhihuishuSection>("zhihuishu")?
                .resolved(String::new())?
                .report_path,
        ))])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("homework", || DEF)
}
