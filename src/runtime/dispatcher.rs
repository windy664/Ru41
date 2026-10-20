use crate::{
    domain::Task,
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::Result;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
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
            let id = format!("task-{}-{}", task.id, task.attempt_count);
            let current = self.store.document("delivery", &id)?;
            let now = crate::runtime::worker::now();
            if let Some(doc) = &current {
                if doc.value["status"] == "sent" {
                    self.store.mark_notified(task.id)?;
                    continue;
                }
                if doc.value["next_at"].as_i64().unwrap_or(0) > now {
                    continue;
                }
            }
            let tries = current
                .as_ref()
                .and_then(|d| d.value["tries"].as_u64())
                .unwrap_or(0)
                + 1;
            let owner = format!("{}", rand::random::<u64>());
            let mut record = serde_json::json!({"task_id":task.id,"channel":route.channel,"target":route.target,"status":"sending","owner":owner,"tries":tries,"next_at":now+60});
            if !self.store.put_document(
                "delivery",
                &id,
                current.as_ref().map(|d| d.revision),
                &record,
            )? {
                continue;
            }
            let revision = current.as_ref().map(|d| d.revision + 1).unwrap_or(1);
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                channel.send(&route.target, route.msg_id.as_deref(), &notify_text(&task)),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    record["status"] = "sent".into();
                    if self
                        .store
                        .put_document("delivery", &id, Some(revision), &record)?
                    {
                        self.store.mark_notified(task.id)?;
                        sent += 1;
                    }
                }
                _ => {
                    record["status"] = "retry_scheduled".into();
                    record["next_at"] = (now + (1_i64 << tries.min(10)).min(3600)).into();
                    self.store
                        .put_document("delivery", &id, Some(revision), &record)?;
                    eprintln!("[notify] delivery {id} failed or timed out");
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
            "## 任务失败\n\n{}",
            task.last_error.as_deref().unwrap_or("未知错误")
        ),
    }
}
