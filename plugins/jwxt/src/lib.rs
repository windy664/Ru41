pub mod captcha;
mod captcha_templates;
pub mod cli;
pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::JwxtSection;
pub use ru41::domain::timetable::{
    CourseMeeting, SectionClock, StoredMeeting, TimetableSnapshot, courses_summary_from_store,
    describe_day_from_store, infer_term, meetings_on, resolve_term, week_of,
};
pub use qq_command::JwxtQqCommand;
pub use service::{JwxtConfig, JwxtPollReport, JwxtTracker};
pub use source::{JwxtClient, JwxtError};

use anyhow::Context as _;
use anyhow::Result;
use std::sync::Arc;

/// 课表 tracker 的具体类型构造：DEF 注册表与 main.rs 的 login 分支共用。
pub fn build_tracker(
    config: &ru41::bootstrap::config::Config,
    store: Arc<dyn ru41::ports::storage::TaskStore>,
) -> Result<JwxtTracker> {
    let jwxt = config
        .sources
        .get::<crate::JwxtSection>("jwxt")?;
    let (notifier, notify_target) = ru41::bootstrap::assembly::notify_pair(config, jwxt.notify)?;
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
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "jwxt",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("jwxt"),
    schedule: |config| {
        let jwxt = config
            .sources
            .get_or_default::<crate::JwxtSection>("jwxt");
        let todo_route_ready = ru41::bootstrap::assembly::default_notify_route(config)
            .ok()
            .flatten()
            .is_some();
        vec![
            ru41::modules::JobSchedule::simple(
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
            ru41::modules::JobSchedule {
                run_command: Some("schedule run class-remind"),
                ..ru41::modules::JobSchedule::simple(
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
        let jwxt = config
            .sources
            .get::<crate::JwxtSection>("jwxt")?;
        match name {
            "jwxt" => Ok(Some(Box::new(build_tracker(config, store)?))),
            "class-remind" => {
                jwxt.is_ready().then_some(()).context(
                    "class-remind requires sources.jwxt credentials (RU41_JWXT_USERNAME/RU41_JWXT_PASSWORD)",
                )?;
                let route = ru41::bootstrap::assembly::default_notify_route(config)?.context(
                    "class-remind requires a default notify route ([notify] or --target)",
                )?;
                let notifier =
                    ru41::bootstrap::assembly::default_notifier(config, &route.channel)?;
                Ok(Some(Box::new(
                    ru41::app::schedule::ClassRemindTracker::new(
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
                .get::<crate::JwxtSection>("jwxt")?
                .semester_start
                .as_deref()
                .and_then(|raw| chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").ok()),
            config
                .sources
                .get::<crate::JwxtSection>("jwxt")?
                .section_clock()
                .ok()
                .flatten(),
        ))])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: jwxt_cli,
};

/// 专属 CLI：`ru41 jwxt classes [today|tomorrow|week|YYYY-MM-DD]` 与
/// `ru41 jwxt login [--captcha x]`（CAS 图形验证码只能人工，两步式）。
fn jwxt_cli(
    config: &ru41::bootstrap::config::Config,
    store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<serde_json::Value>>> + Send>>
{
    use anyhow::Context as _;
    enum Job {
        Classes(crate::JwxtSection, String),
        Login(crate::JwxtTracker, Option<String>),
    }
    let prepared = (|| -> anyhow::Result<Job> {
        match args.first().map(String::as_str) {
            Some("classes") => Ok(Job::Classes(
                config.sources.get::<crate::JwxtSection>("jwxt")?,
                args.get(1).cloned().unwrap_or_else(|| "today".into()),
            )),
            Some("login") => {
                let mut captcha = None;
                let mut rest = args.iter().skip(1);
                while let Some(flag) = rest.next() {
                    match flag.as_str() {
                        "--captcha" => {
                            captcha = Some(
                                rest.next()
                                    .map(String::clone)
                                    .with_context(|| format!("{flag} 缺值"))?,
                            )
                        }
                        other => anyhow::bail!("未知参数 {other}（支持 --captcha）"),
                    }
                }
                Ok(Job::Login(build_tracker(config, store.clone())?, captcha))
            }
            _ => anyhow::bail!(
                "用法: ru41 jwxt classes [today|tomorrow|week|YYYY-MM-DD] | login [--captcha x]"
            ),
        }
    })();
    Box::pin(ru41::plugins::bridge(async move {
        let value = match prepared? {
            Job::Classes(section, when) => cli::classes_json(store, &section, &when)?,
            Job::Login(tracker, captcha) => cli::login_json(&tracker, captcha.as_deref()).await?,
        };
        Ok(Some(value))
    }))
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("jwxt", || DEF)
}
