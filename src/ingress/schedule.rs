use anyhow::Result;
use std::{future::Future, time::Duration};
use tokio::sync::watch;

/// 定时入口的公共执行循环（设计文档 8.2 定时器入口的统一机制）：
/// 立即执行一轮后按间隔循环，失败只记日志等下一轮，收到 shutdown 优雅退出。
/// 单轮逻辑由各调用方保证幂等；系统 cron 场景用对应的 poll 子命令直接调用。
///
/// 各来源 tracker 无需实现任何 trait，传入名称、间隔与轮询闭包即可：
/// ```ignore
/// let tracker = paper_tracker(&config, store)?;
/// let tracker_ref = &tracker; // 闭包按值复制引用，避免借用闭包自身
/// run_periodic("papers", tracker.interval(), move || {
///     let tracker = tracker_ref;
///     async move { Ok(serde_json::to_value(tracker.poll_once().await?)?) }
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
