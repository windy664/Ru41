pub mod config;
pub mod service;
pub mod source;

pub use config::ChaoxingSection;
pub use service::{
    ChaoxingConfig, ChaoxingPollReport, ChaoxingQrSigner, ChaoxingTracker, ChaoxingWorkPollReport,
    ChaoxingWorkTracker, sign_type_name,
};
pub use source::{
    ChaoxingActivity, ChaoxingClient, ChaoxingCourse, ChaoxingWork, CourseEncs, SignMethod,
    fallback_photo_bytes,
};

/// 注册表定义：chaoxing 的调度清单、两个 tracker（签到/作业）都在本模块内闭环。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "chaoxing",
    schedule: |config| {
        let chaoxing = &config.sources.chaoxing;
        vec![
            super::JobSchedule::simple(
                "chaoxing",
                chaoxing
                    .interval_secs
                    .unwrap_or(config::ChaoxingSection::DEFAULT_INTERVAL_SECS),
                chaoxing.is_ready(),
                (!chaoxing.is_ready()).then(|| {
                    "需 sources.chaoxing 手机号与密码（或 RU41_XXT_PHONE/RU41_XXT_PASSWORD）".into()
                }),
            ),
            super::JobSchedule {
                poll_command: Some("chaoxing work"),
                run_command: Some("schedule run chaoxing-work"),
                ..super::JobSchedule::simple(
                    "chaoxing-work",
                    chaoxing
                        .work_poll_secs
                        .unwrap_or(config::ChaoxingSection::DEFAULT_WORK_POLL_SECS),
                    chaoxing.is_ready(),
                    (!chaoxing.is_ready()).then(|| {
                        "随 sources.chaoxing 凭证启用；作业轮询独立低频（默认 30 分钟）".into()
                    }),
                )
            },
        ]
    },
    tracker: |config, store, name| {
        let chaoxing = &config.sources.chaoxing;
        match name {
            "chaoxing" => {
                let (notifier, notify_target) =
                    crate::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
                let resolved = chaoxing.resolved(notify_target)?;
                let client = ChaoxingClient::new(
                    resolved.phone.clone(),
                    resolved.password.clone(),
                    resolved.cookie_path.clone(),
                );
                Ok(Some(Box::new(ChaoxingTracker::new(
                    store, client, notifier, resolved,
                ))))
            }
            "chaoxing-work" => {
                let (notifier, notify_target) =
                    crate::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
                let resolved = chaoxing.resolved(notify_target.clone())?;
                let vault = crate::bootstrap::assembly::vault_if_ready(
                    config,
                    resolved.work_vault_sync,
                )?;
                Ok(Some(Box::new(ChaoxingWorkTracker::new(
                    store,
                    ChaoxingClient::new(
                        resolved.phone.clone(),
                        resolved.password.clone(),
                        resolved.cookie_path.clone(),
                    ),
                    notifier,
                    resolved,
                    config.todo.options()?,
                    vault,
                ))))
            }
            _ => Ok(None),
        }
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};
