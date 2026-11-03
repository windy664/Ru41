pub mod qq_command;
pub use qq_command::TodoQqCommand;

/// 注册表定义：todo-remind 调度（CLI `todo remind` 驱动，无 schedule run 装配）与 /待办 命令。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "planner",
    priority: super::DEFAULT_PRIORITY,
    config_key: None,
    schedule: |config| {
        let todo_route_ready = crate::bootstrap::assembly::default_notify_route(config)
            .ok()
            .flatten()
            .is_some();
        vec![super::JobSchedule {
            poll_command: Some("todo remind --once"),
            run_command: Some("todo remind"),
            ..super::JobSchedule::simple(
                "todo-remind",
                60,
                todo_route_ready,
                if todo_route_ready {
                    Some("间隔可用 --interval-secs 覆盖".into())
                } else {
                    Some("需 [notify] 默认出口，或 schedule run --target 显式指定".into())
                },
            )
        }]
    },
    tracker: |_, _, _| Ok(None),
    qq_handlers: |config, _| {
        // /日程 的合并视图需要按周次过滤课表。只取 semester_start 一个字段，
        // 用本地最小 section 结构（serde 忽略未知键），不依赖 jwxt 的完整类型。
        #[derive(Default, serde::Deserialize)]
        struct JwxtSemesterRef {
            semester_start: Option<String>,
        }
        let jwxt_semester_start = config
            .sources
            .get::<JwxtSemesterRef>("jwxt")?
            .semester_start
            .as_deref()
            .and_then(|raw| chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok());
        Ok(vec![std::sync::Arc::new(TodoQqCommand::new(
            config.todo.options()?,
            jwxt_semester_start,
        ))])
    },
    image_consumers: super::no_image_consumers,
    cli: super::no_cli,
};
