//! 插件 async 边界桥：**dylib 各静态链接一份 tokio，宿主的 reactor 对插件
//! 代码不可见**——插件里的 reqwest/tokio 调用若被宿主 runtime 驱动，
//! 直接 panic "no reactor running" 并 abort 整个进程（2026-10-27 实测）。
//!
//! 插件的 PollJob::poll_json / QQ handler 等返回 future 的入口，必须把
//! 真实工作经此桥交给**插件自己的 runtime**（本函数被插件静态链接后，
//! 实例化的就是插件副本的 tokio），宿主只等一个无 reactor 依赖的 oneshot。
//! 桥内 catch_unwind：插件 panic 降级为 Err，不带崩宿主进程。

use anyhow::Result;
use std::sync::OnceLock;

pub fn bridge<F, T>(
    fut: F,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send>>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    use futures_util::FutureExt;
    static PLUGIN_RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    let rt =
        PLUGIN_RT.get_or_init(|| tokio::runtime::Runtime::new().expect("plugin tokio runtime"));
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.spawn(async move {
        let result = std::panic::AssertUnwindSafe(fut).catch_unwind().await;
        let out = match result {
            Ok(value) => value,
            Err(_) => Err(anyhow::anyhow!("plugin task panicked")),
        };
        let _ = tx.send(out);
    });
    Box::pin(async move {
        rx.await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("plugin task aborted")))
    })
}
