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
            // 雷达日报空报不推送：digest 判"无值得关注"时静默消化。
            // prompt 的"只回一行 📭"纪律 LLM 不总遵守（会展开舍弃说明），程序兜底。
            if task.request_key.starts_with("rss:digest:")
                && task
                    .result
                    .as_ref()
                    .is_some_and(|r| is_quiet_digest(&r.summary))
            {
                self.store.mark_notified(task.id)?;
                continue;
            }
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
                channel.send(
                    &route.target,
                    effective_msg_id(&task, now),
                    &notify_text(&task),
                ),
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
                channel.send(
                    &route.target,
                    effective_msg_id(&task, crate::runtime::worker::now()),
                    &text,
                ),
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
                channel.send(
                    &route.target,
                    effective_msg_id(&task, crate::runtime::worker::now()),
                    &text,
                ),
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
        // 错误原文可能很长（堆栈/HTTP body），只带首行摘要，详情进任务记录查。
        let error = doc.value["error"].as_str().unwrap_or("未知错误");
        let brief = crate::util::truncate_chars(error.lines().next().unwrap_or("未知错误"), 80);
        format!(
            "## ⚠️ 任务 `#{}` 首次执行失败\n\n已自动安排重试，结果出来会通知你。\n\n> {brief}",
            task.id
        )
    } else {
        format!("▶️ 任务 `#{}` 开始执行", task.id)
    }
}

/// 雷达日报"空报"判定：摘要开头含 📭 或"无/没有值得关注"即视为空报。
/// 只看前 80 字：正文里提到这些词（如"其余条目无值得关注的"）不算空报。
fn is_quiet_digest(summary: &str) -> bool {
    let head: String = summary.chars().take(80).collect();
    head.contains('📭') || head.contains("无值得关注") || head.contains("没有值得关注")
}

/// 通知文本：成功用结果摘要，失败带原因。不加脚手架页脚——
/// 用户要的是原生 codex 输出，不是 ru41 的机械说明。
fn notify_text(task: &Task) -> String {
    match &task.result {
        Some(result) => result.summary.clone(),
        None => format!(
            "## 任务失败\n\n{}",
            task.last_error.as_deref().unwrap_or("未知错误")
        ),
    }
}

/// QQ 被动回复窗口：msg_id 只在消息到达后约 5 分钟内有效（CodeX-to-QQ 的
/// PROACTIVE_FINAL_REPLY_AFTER_MS=30000 同款经验），超窗后转主动消息，
/// 否则慢任务的结果会因为过期被动回复而被平台拒绝。
const PASSIVE_WINDOW_SECS: i64 = 240;

/// 超窗的会话把 msg_id 丢掉转主动发送；无 msg_id 或无法解析时间时按原样。
fn effective_msg_id(task: &Task, now: i64) -> Option<&str> {
    let route = task.notify.as_ref()?;
    let msg_id = route.msg_id.as_deref()?;
    let created =
        match chrono::NaiveDateTime::parse_from_str(&task.created_at, "%Y-%m-%dT%H:%M:%S%.fZ") {
            Ok(parsed) => parsed.and_utc().timestamp(),
            // 时间解析不出时不改变既有行为。
            Err(_) => return Some(msg_id),
        };
    if now - created > PASSIVE_WINDOW_SECS {
        None
    } else {
        Some(msg_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{NotifyRoute, TaskStatus};

    #[test]
    fn quiet_digest_detection_looks_at_head_only() {
        assert!(is_quiet_digest("📭 今日无值得关注的机会。"));
        assert!(is_quiet_digest(
            "本批次无值得关注的内容。\n\n**舍弃说明**\n- FreeDNS……"
        ));
        assert!(is_quiet_digest("今天没有值得关注的新条目。"));
        assert!(!is_quiet_digest("本批次筛出 6 条，全部为今日条目。"));
        // 正文深处的提及不算空报
        let mut real = "本批次筛出 2 条值得关注：".repeat(6);
        real.push_str("其余条目无值得关注的内容");
        assert!(!is_quiet_digest(&real));
    }

    fn task(created_at: &str) -> Task {
        Task {
            id: 1,
            request_key: "k".into(),
            backend_id: "codex".into(),
            contract: crate::domain::RunContract::AnalysisV1,
            prompt: "p".into(),
            status: TaskStatus::Queued,
            created_at: created_at.into(),
            updated_at: created_at.into(),
            attempt_count: 0,
            max_attempts: 3,
            active_attempt_id: None,
            lease_until: None,
            available_at: 0,
            result: None,
            last_error: None,
            notify: Some(NotifyRoute {
                channel: "qq".into(),
                target: "user:U1".into(),
                msg_id: Some("m1".into()),
            }),
            notified_at: None,
        }
    }

    #[test]
    fn passive_msg_id_kept_within_window_and_dropped_after() {
        // created_at 是 ISO-UTC；窗口内保留被动锚点，超窗转主动。
        let task = task("2026-01-01T00:00:00.000Z");
        let created =
            chrono::NaiveDateTime::parse_from_str(&task.created_at, "%Y-%m-%dT%H:%M:%S%.fZ")
                .unwrap()
                .and_utc()
                .timestamp();
        assert_eq!(effective_msg_id(&task, created + 60), Some("m1"));
        assert_eq!(
            effective_msg_id(&task, created + PASSIVE_WINDOW_SECS + 1),
            None
        );
    }

    #[test]
    fn msg_id_passthrough_when_unparseable_or_absent() {
        let mut task = task("garbage");
        assert_eq!(effective_msg_id(&task, 999), Some("m1"));
        task.notify.as_mut().unwrap().msg_id = None;
        assert_eq!(effective_msg_id(&task, 999), None);
    }
}
