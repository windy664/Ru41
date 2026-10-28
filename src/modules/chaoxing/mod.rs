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

use crate::bootstrap::config::Config;
use crate::ports::storage::TaskStore;
use anyhow::Result;
use std::sync::Arc;

/// 签到 tracker 的具体类型构造：DEF 注册表与 main.rs 的 sign/login 分支共用。
pub fn build_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<ChaoxingTracker> {
    let chaoxing = config
        .sources
        .get::<crate::modules::chaoxing::ChaoxingSection>("chaoxing")?;
    let (notifier, notify_target) =
        crate::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
    let resolved = chaoxing.resolved(notify_target)?;
    let client = ChaoxingClient::new(
        resolved.phone.clone(),
        resolved.password.clone(),
        resolved.cookie_path.clone(),
    );
    Ok(ChaoxingTracker::new(store, client, notifier, resolved))
}

/// 作业 tracker 的具体类型构造：DEF 注册表与 main.rs 的 work 分支共用。
pub fn build_work_tracker(
    config: &Config,
    store: Arc<dyn TaskStore>,
) -> Result<ChaoxingWorkTracker> {
    let chaoxing = config
        .sources
        .get::<crate::modules::chaoxing::ChaoxingSection>("chaoxing")?;
    let (notifier, notify_target) =
        crate::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
    let resolved = chaoxing.resolved(notify_target.clone())?;
    let vault = crate::bootstrap::assembly::vault_if_ready(config, resolved.work_vault_sync)?;
    Ok(ChaoxingWorkTracker::new(
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
    ))
}

/// 注册表定义：chaoxing 的调度清单、两个 tracker（签到/作业）都在本模块内闭环。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "chaoxing",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("chaoxing"),
    schedule: |config| {
        let chaoxing = config
            .sources
            .get_or_default::<crate::modules::chaoxing::ChaoxingSection>("chaoxing");
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
    tracker: |config, store, name| match name {
        "chaoxing" => Ok(Some(Box::new(build_tracker(config, store)?))),
        "chaoxing-work" => Ok(Some(Box::new(build_work_tracker(config, store)?))),
        _ => Ok(None),
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: |config| {
        // QQ 图片消费者：把二维码照片发给机器人即签（enc 只在二维码图片里）。
        // 与签到轮询各用一份 cookie 文件，避免两进程争写同一会话文件。
        let section = config
            .sources
            .get::<crate::modules::chaoxing::ChaoxingSection>("chaoxing")?;
        if !section.is_ready() {
            return Ok(Vec::new());
        }
        let cfg = section.resolved(String::new())?;
        let client = ChaoxingClient::new(
            cfg.phone.clone(),
            cfg.password.clone(),
            cfg.cookie_path.with_extension("qr.json"),
        );
        Ok(vec![std::sync::Arc::new(ChaoxingQrSigner::new(
            client,
            cfg.default_location.clone(),
            cfg.enc_sample_path.clone(),
        ))])
    },
    cli: super::no_cli,
};
