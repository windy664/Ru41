pub mod cli;
pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::{TeachermateConfig, TeachermateSection};
// TODO(同学重构中): TeachermateCli/TeachermateCommand 尚未在 cli.rs 实现，先不导出
pub use qq_command::TeachermateQqCommand;
pub use service::{
    TeachermatePollReport, TeachermateTracker, TeachermateWorkPollReport, TeachermateWorkTracker,
};
pub use source::{
    ActiveSign, CourseHomework, Homework, HomeworkCourse, SignsOutcome, StudentRole,
    TeachermateClient, homework_status_label, homework_status_open, sign_response_ok,
};

/// 注册表定义：teachermate 的调度清单、两个 tracker（签到/作业）、QQ 命令闭环。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "teachermate",
    schedule: |config| {
        let tm = &config.sources.teachermate;
        vec![
            super::JobSchedule::simple(
                "teachermate",
                tm.interval_secs
                    .unwrap_or(config::TeachermateSection::DEFAULT_INTERVAL_SECS),
                tm.is_ready(),
                (!tm.is_ready()).then(|| {
                    "需微助教 openid：`teachermate bind <链接>` 落盘（或 RU41_WZJ_OPENID）".into()
                }),
            ),
            super::JobSchedule {
                poll_command: Some("teachermate work"),
                run_command: Some("schedule run teachermate-work"),
                ..super::JobSchedule::simple(
                    "teachermate-work",
                    tm.work_poll_secs
                        .unwrap_or(config::TeachermateSection::DEFAULT_WORK_POLL_SECS),
                    tm.is_ready(),
                    (!tm.is_ready()).then(|| {
                        "随 sources.teachermate openid 启用；作业轮询独立低频（默认 30 分钟）"
                            .into()
                    }),
                )
            },
        ]
    },
    tracker: |config, store, name| {
        let tm = &config.sources.teachermate;
        match name {
            "teachermate" => {
                let (notifier, notify_target) =
                    crate::bootstrap::assembly::notify_pair(config, tm.notify)?;
                let resolved = tm.resolved(notify_target)?;
                let client = TeachermateClient::new(resolved.openid.clone())
                    .with_openid_file(resolved.openid_path.clone());
                Ok(Some(Box::new(TeachermateTracker::new(
                    store, client, notifier, resolved,
                ))))
            }
            "teachermate-work" => {
                let (notifier, notify_target) =
                    crate::bootstrap::assembly::notify_pair(config, tm.notify)?;
                let resolved = tm.resolved(notify_target.clone())?;
                let vault = crate::bootstrap::assembly::vault_if_ready(
                    config,
                    resolved.work_vault_sync,
                )?;
                Ok(Some(Box::new(TeachermateWorkTracker::new(
                    store,
                    TeachermateClient::new(resolved.openid.clone())
                        .with_openid_file(resolved.openid_path.clone()),
                    notifier,
                    resolved,
                    config.todo.options()?,
                    vault,
                ))))
            }
            _ => Ok(None),
        }
    },
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(
            qq_command::TeachermateQqCommand::new(
                config.sources.teachermate.openid_path_or_default(),
            ),
        )])
    },
};
