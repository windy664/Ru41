mod captcha;
mod captcha_templates;
pub mod cli;
pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::JwxtSection;
pub use qq_command::JwxtQqCommand;
pub use service::{
    JwxtConfig, JwxtPollReport, JwxtTracker, SectionClock, describe_day_from_store, infer_term,
    meetings_on, resolve_term, week_of,
};
pub use source::{CourseMeeting, JwxtClient, JwxtError};

use anyhow::Context as _;
use anyhow::Result;
use std::sync::Arc;

/// 课表 tracker 的具体类型构造：DEF 注册表与 main.rs 的 login 分支共用。
pub fn build_tracker(
    config: &crate::bootstrap::config::Config,
    store: Arc<dyn crate::ports::storage::TaskStore>,
) -> Result<JwxtTracker> {
    let jwxt = &config.sources.jwxt;
    let (notifier, notify_target) = crate::bootstrap::assembly::notify_pair(config, jwxt.notify)?;
    let resolved = jwxt.resolved(notify_target)?;
    let client = JwxtClient::new(
        resolved.username.clone(),
        resolved.password.clone(),
        resolved.cas_base.clone(),
        resolved.cas_modulus.clone(),
        resolved.cas_exponent.clone(),
        resolved.jwxt_base.clone(),
        resolved.session_path.clone(),
    )
    .with_browser_ua();
    Ok(JwxtTracker::new(store, client, notifier, resolved))
}

/// 注册表定义：jwxt 的调度清单、课表/课前提醒两个 tracker、QQ 命令闭环。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "jwxt",
    schedule: |config| {
        let jwxt = &config.sources.jwxt;
        let todo_route_ready = crate::bootstrap::assembly::default_notify_route(config)
            .ok()
            .flatten()
            .is_some();
        vec![
            super::JobSchedule::simple(
                "jwxt",
                jwxt.interval_secs
                    .unwrap_or(config::JwxtSection::DEFAULT_INTERVAL_SECS),
                jwxt.is_ready(),
                if !jwxt.is_ready() {
                    Some(
                        "需 sources.jwxt 用户名与密码（或 RU41_JWXT_USERNAME/RU41_JWXT_PASSWORD）"
                            .into(),
                    )
                } else if jwxt.semester_start.is_none() {
                    Some("已启用；配置 semester_start（学期第一周周一）后才有明日课表提醒".into())
                } else {
                    None
                },
            ),
            super::JobSchedule {
                run_command: Some("schedule run class-remind"),
                ..super::JobSchedule::simple(
                    "class-remind",
                    60, // 兜底轮询；精准睡眠按 fire_at 计算
                    jwxt.is_ready() && todo_route_ready,
                    if !jwxt.is_ready() {
                        Some("需 sources.jwxt 凭证（随 jwxt 启用）".into())
                    } else if !todo_route_ready {
                        Some("需 [notify] 默认出口，或 schedule run --target 显式指定".into())
                    } else {
                        Some("课前 20 分钟提醒；启动时补发停机期间错过的课".into())
                    },
                )
            },
        ]
    },
    tracker: |config, store, name| {
        let jwxt = &config.sources.jwxt;
        match name {
            "jwxt" => Ok(Some(Box::new(build_tracker(config, store)?))),
            "class-remind" => {
                jwxt.is_ready().then_some(()).context(
                    "class-remind requires sources.jwxt credentials (RU41_JWXT_USERNAME/RU41_JWXT_PASSWORD)",
                )?;
                let route = crate::bootstrap::assembly::default_notify_route(config)?.context(
                    "class-remind requires a default notify route ([notify] or --target)",
                )?;
                let notifier =
                    crate::bootstrap::assembly::default_notifier(config, &route.channel)?;
                Ok(Some(Box::new(
                    crate::app::schedule::ClassRemindTracker::new(
                        store,
                        notifier,
                        route,
                        config.todo.options()?,
                    ),
                )))
            }
            _ => Ok(None),
        }
    },
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(JwxtQqCommand::new(
            config
                .sources
                .jwxt
                .semester_start
                .as_deref()
                .and_then(|raw| chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()),
            config.sources.jwxt.section_clock().ok().flatten(),
        ))])
    },
};
