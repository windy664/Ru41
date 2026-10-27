use anyhow::Result;
use serde::Serialize;
use std::{future::Future, pin::Pin, time::Duration};
use tokio::sync::watch;

/// 定时任务注册表条目：各附属模块单轮 poll 与长驻 run 的统一描述。
/// 注册表只描述任务与入口，执行逻辑仍由各模块自己的 poll_once 提供；
/// 新附属在装配层登记一个条目即可被 `schedule list/run` 统一调度。
#[derive(Debug, Clone, Serialize)]
pub struct ScheduledJob {
    /// 所属模块（papers/rss/ecard/chaoxing/jwxt/zhihuishu/planner）。
    pub module: String,
    /// 任务名，也是 `schedule run` 的参数。
    pub name: String,
    /// 长驻入口的轮询间隔（秒）；None 表示间隔由命令行参数或外部 cron 决定。
    pub interval_secs: Option<u64>,
    /// 单轮入口，适合系统 cron 或手动验证。
    pub poll_command: String,
    /// 长驻入口，适合 compose 常驻容器。
    pub run_command: String,
    /// 当前配置是否可直接启动。
    pub enabled: bool,
    /// 未启用原因或额外参数说明。
    pub notes: Option<String>,
}

/// 附属追踪器对调度核心的统一暴露：单轮 poll 逻辑与业务留在附属，
/// 调度语义（间隔、报告序列化）归核心；模块 `run` 命令与 `schedule run` 共用。
pub trait PollJob: Send + Sync {
    fn job_interval(&self) -> Duration;
    fn poll_json(&self) -> Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + '_>>;
}

/// 定时入口的公共执行循环（设计文档 8.2 定时器入口的统一机制）：
/// 立即执行一轮后按间隔循环，失败只记日志等下一轮，收到 shutdown 优雅退出。
/// 单轮逻辑由各调用方保证幂等；系统 cron 场景用对应的 poll 子命令直接调用。
///
/// 各来源 tracker 无需实现任何 trait，传入名称、间隔与轮询闭包即可：
/// ```ignore
/// let tracker = paper_tracker(&config, store)?;
/// let tracker_ref = &tracker; // 闭包按值复制引用，避免借用闭包自身
/// run_periodic("papers", tracker_ref.job_interval(), move || {
///     let tracker = tracker_ref;
///     async move { tracker.poll_json().await }
/// }, shutdown).await?;
/// ```
pub async fn run_periodic<F, Fut>(
    name: &str,
    interval: Duration,
    poll: F,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<serde_json::Value>>,
{
    // 间隔下限兜底：配置写成 0/过小时，无 sleep 的紧密循环会把宿主机 CPU 打满
    // （有过 obsidian 容器把笔记本烧到热保护的先例），这里强制抬到 30s。
    let interval = if interval < Duration::from_secs(30) {
        eprintln!("[{name}] interval {interval:?} 过小，已抬到 30s 下限");
        Duration::from_secs(30)
    } else {
        interval
    };
    loop {
        match poll().await {
            Ok(report) => println!("{}", serde_json::to_string(&report)?),
            Err(error) => eprintln!("[{name}] poll failed: {error:#}"),
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(interval) => {}
        }
    }
    Ok(())
}
