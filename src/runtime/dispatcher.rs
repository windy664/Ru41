use crate::{
    domain::{Task, TaskStatus},
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::Result;
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
/// 终态任务的渠道通知 + 进程内进度通知（started/retry）：
/// 轮询未通知任务与 pending 进度文档，按 notify_channel 路由，送达后标记。
/// 发送失败不标记，下一轮重试；未注册的渠道跳过（留待对应网关上线）。
pub struct NotifyDispatcher {
    pub store: Arc<dyn TaskStore>,
    pub channels: Vec<Arc<dyn NotifyChannel>>,
    pub batch: u32,
    pub interval: Duration,
}

impl NotifyDispatcher {
    /// 处理一批终态通知、进度通知与活动流 chunk，返回成功送达的总数。
    pub async fn run_once(&self) -> Result<usize> {
        let sent = self.dispatch_terminal().await?;
        Ok(sent + self.dispatch_progress().await? + self.dispatch_activity().await?)
    }

    /// 终态任务通知：成功用结果摘要（带页脚），失败带原因。
    async fn dispatch_terminal(&self) -> Result<usize> {
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

    /// 进度通知：worker 写入的 pending「开始执行/首次失败重试」文档。
    /// 任务已终态或通知渠道缺失时直接标记消化，避免残留。
    async fn dispatch_progress(&self) -> Result<usize> {
        let docs = self.store.documents("progress", self.batch)?;
        let mut sent = 0;
        for doc in docs {
            if doc.value["status"] != "pending" {
                continue;
            }
            let Some(task_id) = doc.value["task_id"].as_i64() else {
                self.mark_doc("progress", &doc, "invalid")?;
                continue;
            };
            let task = match self.store.get(task_id)? {
                Some(task) => task,
                None => {
                    self.mark_doc("progress", &doc, "orphan")?;
                    continue;
                }
            };
            // 终态任务由终态通知收口，进度通知不再补发。
            let terminal = matches!(
                task.status,
                TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
            );
            let Some(route) = &task.notify else {
                self.mark_doc("progress", &doc, "done")?;
                continue;
            };
            if terminal {
                self.mark_doc("progress", &doc, "done")?;
                continue;
            }
            let Some(channel) = self
                .channels
                .iter()
                .find(|c| c.channel_id() == route.channel)
            else {
                continue;
            };
            let text = progress_text(&doc, &task);
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                channel.send(&route.target, route.msg_id.as_deref(), &text),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    if self.mark_doc("progress", &doc, "sent")? {
                        sent += 1;
                    }
                }
                _ => eprintln!("[notify] progress {} failed or timed out", doc.id),
            }
        }
        Ok(sent)
    }

    /// 活动流 chunk：worker 从底座 events.jsonl 翻译过来的执行过程（💬💭⚙️🔍），
    /// 逐 chunk 发回原会话，让用户像看 codex 终端一样看到 agent 在做什么。
    /// 任务已终态时剩余 chunk 直接消化，不打断终态结果。
    async fn dispatch_activity(&self) -> Result<usize> {
        let docs = self.store.documents("activity", self.batch)?;
        let mut sent = 0;
        for doc in docs {
            if doc.value["status"] != "pending" {
                continue;
            }
            let Some(task_id) = doc.value["task_id"].as_i64() else {
                self.mark_doc("activity", &doc, "invalid")?;
                continue;
            };
            let Some(task) = self.store.get(task_id)? else {
                self.mark_doc("activity", &doc, "orphan")?;
                continue;
            };
            let Some(route) = &task.notify else {
                self.mark_doc("activity", &doc, "done")?;
                continue;
            };
            // 终态收口后不再补发流式过程，避免晚到的 chunk 盖过最终结果。
            let terminal = matches!(
                task.status,
                TaskStatus::Succeeded | TaskStatus::Failed | TaskStatus::Cancelled
            );
            if terminal {
                self.mark_doc("activity", &doc, "done")?;
                continue;
            }
            let Some(channel) = self
                .channels
                .iter()
                .find(|c| c.channel_id() == route.channel)
            else {
                continue;
            };
            let lines: Vec<&str> = doc.value["lines"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|line| line.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let text = lines.join("\n");
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                channel.send(&route.target, route.msg_id.as_deref(), &text),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    if self.mark_doc("activity", &doc, "sent")? {
                        sent += 1;
                    }
                }
                _ => eprintln!("[notify] activity {} failed or timed out", doc.id),
            }
        }
        Ok(sent)
    }

    /// 把文档标记为指定状态；集合名必须与读取时的 kind 一致（ Revision CAS ）。
    fn mark_doc(
        &self,
        collection: &str,
        doc: &crate::domain::records::Document,
        status: &str,
    ) -> Result<bool> {
        let mut value = doc.value.clone();
        value["status"] = status.into();
        self.store
            .put_document(collection, &doc.id, Some(doc.revision), &value)
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

/// 进度通知文本。
fn progress_text(doc: &crate::domain::records::Document, task: &Task) -> String {
    if doc.value["kind"] == "retry" {
        format!(
            "## ⚠️ 任务 `#{}` 首次执行失败\n\n已自动安排重试，结果出来会通知你。\n\n错误：\n{}",
            task.id,
            doc.value["error"].as_str().unwrap_or("未知错误")
        )
    } else {
        let attempt = doc.value["attempt"]
            .as_i64()
            .or_else(|| doc.value["attempt_id"].as_i64())
            .unwrap_or(1);
        format!(
            "▶️ 任务 `#{}` 开始执行（第 {} 次尝试 · 底座 `{}`）",
            task.id, attempt, task.backend_id
        )
    }
}

/// 通知文本：成功用结果摘要，失败带原因；都附页脚说明任务身份与开销。
fn notify_text(task: &Task) -> String {
    let body = match &task.result {
        Some(result) => result.summary.clone(),
        None => format!(
            "## 任务失败\n\n{}",
            task.last_error.as_deref().unwrap_or("未知错误")
        ),
    };
    format!("{body}\n\n{}", task_footer(task))
}

/// 终态回复页脚：任务号、底座、尝试次数、用时，消除「这是哪条任务」的黑盒感。
fn task_footer(task: &Task) -> String {
    let mut parts = vec![
        format!("任务 `#{}`", task.id),
        format!("底座 `{}`", task.backend_id),
        format!("尝试 {} 次", task.attempt_count),
    ];
    if let Some(elapsed) = elapsed_label(&task.created_at, &task.updated_at) {
        parts.push(format!("用时 {elapsed}"));
    }
    format!("—— {}", parts.join(" · "))
}

/// 解析 created_at/updated_at（ISO-UTC，'%Y-%m-%dT%H:%M:%S%.3fZ'）求人类可读时长。
fn elapsed_label(created_at: &str, updated_at: &str) -> Option<String> {
    let parse = |raw: &str| {
        chrono::DateTime::parse_from_rfc3339(raw)
            .map(|dt| dt.timestamp())
            .ok()
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.fZ")
                    .ok()
                    .map(|naive| naive.and_utc().timestamp())
            })
    };
    let secs = parse(updated_at)? - parse(created_at)?;
    if secs < 0 {
        return None;
    }
    if secs < 60 {
        Some(format!("{secs} 秒"))
    } else if secs < 3600 {
        Some(format!("{} 分钟", secs / 60))
    } else {
        Some(format!("{} 小时 {} 分钟", secs / 3600, (secs % 3600) / 60))
    }
}
