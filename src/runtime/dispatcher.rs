use crate::{
    domain::{Task, TaskStatus, records::Interaction},
    ports::{
        channel::NotifyChannel,
        commands::{MarkupButton, ReplyMarkup},
        storage::TaskStore,
    },
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
    /// 静默来源（后台服务）：这些任务不向用户汇报——终态通知、进度通知、活动流全不推，
    /// 但任务照跑、产物照写。匹配 request_key 的前缀段。
    pub silent_sources: Vec<String>,
}

/// request_key 是否命中静默来源（前缀段匹配，段数不限）。
pub fn is_silent_source(request_key: &str, silent_sources: &[String]) -> bool {
    silent_sources.iter().any(|entry| {
        let entry = entry.trim();
        !entry.is_empty()
            && (request_key == entry
                || request_key
                    .strip_prefix(entry)
                    .is_some_and(|rest| rest.starts_with(':')))
    })
}

impl NotifyDispatcher {
    /// 处理一批终态通知、进度通知与活动流 chunk，返回成功送达的总数。
    pub async fn run_once(&self) -> Result<usize> {
        let sent = self.dispatch_terminal().await?;
        Ok(sent
            + self.dispatch_progress().await?
            + self.dispatch_activity().await?
            + self.dispatch_interactions().await?
            + self.convert_stale_steers().await?)
    }

    /// 孤儿插话兜底：steer 记录超过 30s 仍 pending，说明插话时任务已结束
    /// （运行结束的竞速、worker 崩溃），转成新任务继续处理，不让话落空。
    pub async fn convert_stale_steers(&self) -> Result<usize> {
        let docs = self.store.documents("steer", self.batch)?;
        let now = crate::util::now();
        let mut converted = 0;
        for doc in docs {
            if doc.value["status"] != "pending"
                || now - doc.value["created_at"].as_i64().unwrap_or(now) < 30
            {
                continue;
            }
            let text = doc.value["text"].as_str().unwrap_or_default();
            let route: Option<crate::domain::NotifyRoute> =
                serde_json::from_value(doc.value["notify"].clone()).unwrap_or(None);
            let (Some(route), false) = (route, text.is_empty()) else {
                self.mark_doc("steer", &doc, "invalid")?;
                continue;
            };
            crate::app::task::TaskService::new(self.store.clone()).enqueue_with_notify(
                &format!("steer:requeue:{}", doc.id),
                text,
                "pi",
                &route,
            )?;
            if self.mark_doc("steer", &doc, "requeued")? {
                converted += 1;
            }
        }
        Ok(converted)
    }

    /// 交互卡片：底座在运行中发起的审批/提问，渲染成带按钮的卡片发回
    /// 会话，用户点按钮（代发 `/答复 <编号> <序号>`）或超时由 dismiss 兜底。
    /// 发送成功后把 notified 置位（CAS），失败留待下一轮重试。
    async fn dispatch_interactions(&self) -> Result<usize> {
        let docs = self.store.documents("interaction", self.batch)?;
        let mut sent = 0;
        for doc in docs {
            let interaction: Interaction = match serde_json::from_value(doc.value.clone()) {
                Ok(interaction) => interaction,
                Err(_) => continue,
            };
            if interaction.status != "pending" || interaction.notified {
                continue;
            }
            let Some(route) = &interaction.notify else {
                continue;
            };
            let Some(channel) = self
                .channels
                .iter()
                .find(|c| c.channel_id() == route.channel)
            else {
                continue;
            };
            let (text, labels) = crate::app::interactions::render_card(&interaction);
            let short_id: String = interaction.id.chars().take(8).collect();
            let mut markup = ReplyMarkup::new();
            // 每行 3 个按钮（QQ 键盘行宽经验值），最多 4 行。
            for row in labels
                .iter()
                .enumerate()
                .take(12)
                .collect::<Vec<_>>()
                .chunks(3)
            {
                markup.push_row(
                    row.iter()
                        .map(|(index, label)| {
                            MarkupButton::new(*label, format!("/答复 {short_id} {}", index + 1))
                        })
                        .collect(),
                );
            }
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                channel.send_with_markup(&route.target, route.msg_id.as_deref(), &text, &markup),
            )
            .await;
            match result {
                Ok(Ok(())) => {
                    let mut value = doc.value.clone();
                    value["notified"] = serde_json::json!(true);
                    if self.store.put_document(
                        "interaction",
                        &doc.id,
                        Some(doc.revision),
                        &value,
                    )? {
                        sent += 1;
                    }
                }
                _ => eprintln!("[notify] interaction {} delivery failed", doc.id),
            }
        }
        Ok(sent)
    }

    /// 终态任务通知：成功用结果摘要（带页脚），失败带原因。
    async fn dispatch_terminal(&self) -> Result<usize> {
        let tasks = self.store.list_unnotified(self.batch)?;
        let mut sent = 0;
        for task in tasks {
            let Some(route) = &task.notify else { continue };
            // 后台服务静默：消化掉但不发。
            if is_silent_source(&task.request_key, &self.silent_sources) {
                self.store.mark_notified(task.id)?;
                continue;
            }
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
                    // 结果带媒体（截图/图表/生成文档）时逐个随消息发出。
                    // QQ 会上传成平台媒体；不支持媒体的渠道降级成文本链接。
                    if let Some(result) = task.result.as_ref() {
                        for media in &result.media {
                            let caption = media.display_name().to_owned();
                            let sent_media = tokio::time::timeout(
                                Duration::from_secs(60),
                                channel.send_media(
                                    &route.target,
                                    effective_msg_id(&task, now),
                                    &caption,
                                    media,
                                ),
                            )
                            .await;
                            if !matches!(sent_media, Ok(Ok(()))) {
                                eprintln!("[notify] media delivery failed for task-{}", task.id);
                            }
                        }
                    }
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
            if is_silent_source(&task.request_key, &self.silent_sources) {
                self.mark_doc("progress", &doc, "done")?;
                continue;
            }
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
            if is_silent_source(&task.request_key, &self.silent_sources) {
                self.mark_doc("activity", &doc, "done")?;
                continue;
            }
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

/// 通知里的任务标题：prompt 首行截断，让用户一眼认出失败的是哪件事。
fn task_brief(task: &Task) -> String {
    let first = task.prompt.lines().next().unwrap_or("").trim();
    crate::util::truncate_chars(first, 40)
}

/// 进度通知文本。
fn progress_text(doc: &crate::domain::records::Document, task: &Task) -> String {
    if doc.value["kind"] == "retry" {
        // 错误原文可能很长（堆栈/HTTP body），只带首行摘要，详情进任务记录查。
        let error = doc.value["error"].as_str().unwrap_or("未知错误");
        let brief = crate::util::truncate_chars(error.lines().next().unwrap_or("未知错误"), 80);
        format!(
            "## ⚠️ 任务 `#{}` 首次执行失败\n\n{}\n\n已自动安排重试，结果出来会通知你。\n\n> {brief}",
            task.id,
            task_brief(task),
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
            "## 任务失败\n\n{}\n\n> {}",
            task_brief(task),
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
    fn silent_source_matches_by_prefix_segment() {
        let silent = vec![
            "obsidian".to_string(),
            "rss:opportunities".to_string(),
            "papers".to_string(),
        ];
        // 后台服务：命中。
        assert!(is_silent_source("obsidian:organize:abc123", &silent));
        assert!(is_silent_source("rss:opportunities:2026-11-05", &silent));
        assert!(is_silent_source("papers:digest", &silent));
        // 用户要看的：不能误伤。
        assert!(!is_silent_source("rss:digest:morning:2026-11-05", &silent));
        assert!(!is_silent_source("qq:msg:abc", &silent));
        assert!(!is_silent_source("youtube:prep:x", &silent));
        assert!(!is_silent_source("lecture:notes:x", &silent));
        // 前缀相同但不是段边界，不算命中。
        assert!(!is_silent_source("obsidianly:organize:x", &silent));
        // 空列表不静默任何东西。
        assert!(!is_silent_source("obsidian:organize:x", &[]));
    }

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
