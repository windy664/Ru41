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

use ru41::bootstrap::config::Config;
use ru41::ports::storage::TaskStore;
use anyhow::Result;
use std::sync::Arc;

/// 签到 tracker 的具体类型构造：DEF 注册表与 main.rs 的 bind 分支共用。
pub fn build_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<TeachermateTracker> {
    let tm = config
        .sources
        .get::<crate::TeachermateSection>("teachermate")?;
    let (notifier, notify_target) = ru41::bootstrap::assembly::notify_pair(config, tm.notify)?;
    let resolved = tm.resolved(notify_target)?;
    let client = TeachermateClient::new(resolved.openid.clone())
        .with_openid_file(resolved.openid_path.clone());
    Ok(TeachermateTracker::new(store, client, notifier, resolved))
}

/// 作业 tracker 的具体类型构造：DEF 注册表与 main.rs 的 work 分支共用。
pub fn build_work_tracker(
    config: &Config,
    store: Arc<dyn TaskStore>,
) -> Result<TeachermateWorkTracker> {
    let tm = config
        .sources
        .get::<crate::TeachermateSection>("teachermate")?;
    let (notifier, notify_target) = ru41::bootstrap::assembly::notify_pair(config, tm.notify)?;
    let resolved = tm.resolved(notify_target.clone())?;
    let vault = ru41::bootstrap::assembly::vault_if_ready(config, resolved.work_vault_sync)?;
    Ok(TeachermateWorkTracker::new(
        store,
        TeachermateClient::new(resolved.openid.clone())
            .with_openid_file(resolved.openid_path.clone()),
        notifier,
        resolved,
        config.todo.options()?,
        vault,
    ))
}

/// 注册表定义：teachermate 的调度清单、两个 tracker（签到/作业）、QQ 命令闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "teachermate",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("teachermate"),
    schedule: |config| {
        let tm = config
            .sources
            .get_or_default::<crate::TeachermateSection>("teachermate");
        vec![
            ru41::modules::JobSchedule::simple(
                "teachermate",
                tm.interval_secs
                    .unwrap_or(config::TeachermateSection::DEFAULT_INTERVAL_SECS),
                tm.is_ready(),
                (!tm.is_ready()).then(|| {
                    "需微助教 openid：`teachermate bind <链接>` 落盘（或 RU41_WZJ_OPENID）".into()
                }),
            ),
            ru41::modules::JobSchedule {
                poll_command: Some("teachermate work"),
                run_command: Some("schedule run teachermate-work"),
                ..ru41::modules::JobSchedule::simple(
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
    tracker: |config, store, name| match name {
        "teachermate" => Ok(Some(Box::new(build_tracker(config, store)?))),
        "teachermate-work" => Ok(Some(Box::new(build_work_tracker(config, store)?))),
        _ => Ok(None),
    },
    qq_handlers: |config, _| {
        Ok(vec![std::sync::Arc::new(
            qq_command::TeachermateQqCommand::new(
                config
                    .sources
                    .get::<crate::TeachermateSection>("teachermate")?
                    .openid_path_or_default(),
            ),
        )])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: teachermate_cli,
};

/// 专属 CLI：`ru41 teachermate work|bind <链接>|sign <id> [--lat x --lon y]`。
/// 参数在 async 之前解析成 owned 枚举（cli 钩子返回的 future 必须 'static）。
fn teachermate_cli(
    config: &ru41::bootstrap::config::Config,
    store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Option<serde_json::Value>>> + Send>>
{
    use anyhow::Context as _;
    enum Job {
        Work(TeachermateWorkTracker),
        Bind(String, std::path::PathBuf),
        Sign(TeachermateTracker, i64, Option<f64>, Option<f64>),
    }
    let prepared = (|| -> anyhow::Result<Job> {
        match args.first().map(String::as_str) {
            Some("work") => Ok(Job::Work(build_work_tracker(config, store.clone())?)),
            Some("bind") => {
                let link = args
                    .get(1)
                    .context("用法: ru41 teachermate bind <服务号链接|openid>")?
                    .clone();
                let path = config
                    .sources
                    .get::<crate::TeachermateSection>("teachermate")?
                    .openid_path_or_default();
                Ok(Job::Bind(link, path))
            }
            Some("sign") => {
                let sign_id: i64 = args
                    .get(1)
                    .context("用法: ru41 teachermate sign <id> [--lat x] [--lon y]")?
                    .parse()
                    .context("sign id 必须是数字")?;
                let mut lat = None;
                let mut lon = None;
                let mut rest = args.iter().skip(2);
                while let Some(flag) = rest.next() {
                    match flag.as_str() {
                        "--lat" => {
                            lat = Some(
                                rest.next()
                                    .context("--lat 缺值")?
                                    .parse()
                                    .context("--lat 必须是数字")?,
                            )
                        }
                        "--lon" => {
                            lon = Some(
                                rest.next()
                                    .context("--lon 缺值")?
                                    .parse()
                                    .context("--lon 必须是数字")?,
                            )
                        }
                        other => anyhow::bail!("未知参数 {other}（支持 --lat/--lon）"),
                    }
                }
                Ok(Job::Sign(
                    build_tracker(config, store.clone())?,
                    sign_id,
                    lat,
                    lon,
                ))
            }
            _ => anyhow::bail!("用法: ru41 teachermate work|bind <链接>|sign <id> [--lat x --lon y]"),
        }
    })();
    Box::pin(async move {
        let value = match prepared? {
            Job::Work(tracker) => serde_json::to_value(tracker.poll_once().await?)?,
            Job::Bind(link, path) => cli::bind_json(&link, &path).await?,
            Job::Sign(tracker, sign_id, lat, lon) => {
                serde_json::to_value(tracker.sign_manual(sign_id, lat, lon).await?)?
            }
        };
        Ok(Some(value))
    })
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("teachermate", || DEF)
}
