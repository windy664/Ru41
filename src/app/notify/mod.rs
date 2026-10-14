use crate::{app::store::TaskStore, domain::Task};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::watch;

/// 一个通知渠道（如 QQ）；只负责把终态结果送达 target。
pub trait NotifyChannel: Send + Sync {
    fn channel_id(&self) -> &str;
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

impl<T: NotifyChannel + ?Sized> NotifyChannel for Arc<T> {
    fn channel_id(&self) -> &str {
        (**self).channel_id()
    }
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        (**self).send(target, msg_id, text)
    }
}

/// 终态任务的渠道通知：轮询未通知任务，按 notify_channel 路由，送达后标记。
/// 发送失败不标记，下一轮重试；未注册的渠道跳过（留待对应网关上线）。
pub struct NotifyDispatcher {
    pub store: Arc<dyn TaskStore>,
    pub channels: Vec<Arc<dyn NotifyChannel>>,
    pub batch: u32,
    pub interval: Duration,
}

impl NotifyDispatcher {
    /// 处理一批未通知任务，返回成功通知的数量。
    pub async fn run_once(&self) -> Result<usize> {
        let tasks = self.store.list_unnotified(self.batch)?;
        let mut sent = 0;
        for task in tasks {
            let Some(route) = &task.notify else { continue };
            let Some(channel) = self
                .channels
                .iter()
                .find(|c| c.channel_id() == route.channel)
            else {
                continue;
            };
            match channel
                .send(&route.target, route.msg_id.as_deref(), &notify_text(&task))
                .await
            {
                Ok(()) => {
                    self.store.mark_notified(task.id)?;
                    sent += 1;
                }
                Err(error) => {
                    eprintln!(
                        "[notify] task {} via {} failed: {error:#}",
                        task.id, route.channel
                    );
                }
            }
        }
        Ok(sent)
    }

    pub async fn run_loop(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut poll = tokio::time::interval(self.interval);
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                _ = poll.tick() => { self.run_once().await?; }
            }
        }
    }
}

/// 通知文本：成功用结果摘要，失败带原因。
fn notify_text(task: &Task) -> String {
    match &task.result {
        Some(result) => result.summary.clone(),
        None => format!(
            "任务失败：{}",
            task.last_error.as_deref().unwrap_or("未知错误")
        ),
    }
}
