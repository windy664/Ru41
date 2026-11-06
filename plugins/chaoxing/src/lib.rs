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

use anyhow::Result;
use ru41::bootstrap::config::Config;
use ru41::ports::storage::TaskStore;
use std::sync::Arc;

/// 签到 tracker 的具体类型构造：DEF 注册表与 main.rs 的 sign/login 分支共用。
pub fn build_tracker(config: &Config, store: Arc<dyn TaskStore>) -> Result<ChaoxingTracker> {
    let chaoxing = config.sources.get::<crate::ChaoxingSection>("chaoxing")?;
    let (notifier, notify_target) =
        ru41::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
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
    let chaoxing = config.sources.get::<crate::ChaoxingSection>("chaoxing")?;
    let (notifier, notify_target) =
        ru41::bootstrap::assembly::notify_pair(config, chaoxing.notify)?;
    let resolved = chaoxing.resolved(notify_target.clone())?;
    let vault = ru41::bootstrap::assembly::vault_if_ready(config, resolved.work_vault_sync)?;
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
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "chaoxing",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("chaoxing"),
    schedule: |config| {
        let chaoxing = config
            .sources
            .get_or_default::<crate::ChaoxingSection>("chaoxing");
        vec![
            ru41::modules::JobSchedule::simple(
                "chaoxing",
                chaoxing
                    .interval_secs
                    .unwrap_or(config::ChaoxingSection::DEFAULT_INTERVAL_SECS),
                chaoxing.is_ready(),
                (!chaoxing.is_ready()).then(|| {
                    "需 sources.chaoxing 手机号与密码（或 RU41_XXT_PHONE/RU41_XXT_PASSWORD）".into()
                }),
            ),
            ru41::modules::JobSchedule {
                poll_command: Some("chaoxing work"),
                run_command: Some("schedule run chaoxing-work"),
                ..ru41::modules::JobSchedule::simple(
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
        let section = config.sources.get::<crate::ChaoxingSection>("chaoxing")?;
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
    cli: chaoxing_cli,
};

/// 专属 CLI：`ru41 chaoxing work|sign <活动ID> [--lat x --lon y --addr t --enc e --photo p]`。
/// 参数在 async 之前解析成 owned 枚举（cli 钩子返回的 future 必须 'static）。
fn chaoxing_cli(
    config: &ru41::bootstrap::config::Config,
    store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = anyhow::Result<Option<serde_json::Value>>> + Send>,
> {
    use anyhow::Context as _;
    enum Job {
        Work(ChaoxingWorkTracker),
        Sign(
            ChaoxingTracker,
            String,
            Option<f64>,
            Option<f64>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    }
    let prepared = (|| -> anyhow::Result<Job> {
        match args.first().map(String::as_str) {
            Some("work") => Ok(Job::Work(build_work_tracker(config, store.clone())?)),
            Some("sign") => {
                let activity_id = args
                    .get(1)
                    .context("用法: ru41 chaoxing sign <活动ID> [--lat x] [--lon y] [--addr t] [--enc e] [--photo p]")?
                    .clone();
                let mut lat = None;
                let mut lon = None;
                let mut addr = None;
                let mut enc = None;
                let mut photo = None;
                let mut rest = args.iter().skip(2);
                while let Some(flag) = rest.next() {
                    match flag.as_str() {
                        "--lat" | "--lon" => {
                            let value = rest
                                .next()
                                .cloned()
                                .with_context(|| format!("{flag} 缺值"))?;
                            let parsed: f64 = value
                                .parse()
                                .with_context(|| format!("{flag} 必须是数字"))?;
                            if flag == "--lat" {
                                lat = Some(parsed);
                            } else {
                                lon = Some(parsed);
                            }
                        }
                        "--addr" => {
                            addr = Some(
                                rest.next()
                                    .cloned()
                                    .with_context(|| format!("{flag} 缺值"))?,
                            )
                        }
                        "--enc" => {
                            enc = Some(
                                rest.next()
                                    .cloned()
                                    .with_context(|| format!("{flag} 缺值"))?,
                            )
                        }
                        "--photo" => {
                            photo = Some(
                                rest.next()
                                    .cloned()
                                    .with_context(|| format!("{flag} 缺值"))?,
                            )
                        }
                        other => anyhow::bail!(
                            "未知参数 {other}（支持 --lat/--lon/--addr/--enc/--photo）"
                        ),
                    }
                }
                Ok(Job::Sign(
                    build_tracker(config, store.clone())?,
                    activity_id,
                    lat,
                    lon,
                    addr,
                    enc,
                    photo,
                ))
            }
            _ => anyhow::bail!(
                "用法: ru41 chaoxing work|sign <活动ID> [--lat x] [--lon y] [--addr t] [--enc e] [--photo p]"
            ),
        }
    })();
    Box::pin(ru41::plugins::bridge(async move {
        let value = match prepared? {
            Job::Work(tracker) => serde_json::to_value(tracker.poll_once().await?)?,
            Job::Sign(tracker, activity_id, lat, lon, addr, enc, photo) => serde_json::to_value(
                tracker
                    .sign_manual(
                        &activity_id,
                        lat,
                        lon,
                        addr.as_deref(),
                        enc.as_deref(),
                        photo.as_deref(),
                    )
                    .await?,
            )?,
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
    ru41::plugins::PluginEntry::new("chaoxing", || DEF)
}
