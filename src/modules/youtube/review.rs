//! 复习闭环：预习单词块落库 → 每日间隔复习推送 → 看后测试 → 已会词反哺预习挑词。
//!
//! 预习单只解决「看懂」，学习发生在看之后：本文件实现遗忘对抗的三个机制——
//! 词块按 1/3/7/14/30 天间隔推 QQ 回忆题（SQ 阶梯）；看视频次日推 Agent 后测题；
//! 「会」过的词进已知名单，预习挑词自动排除。
//!
//! records 表数据模型：
//! - `youtube_vocab`：`<video>#<n>` 一词一条（chunk/cue/stage/due_at/known），`<video>#meta` 标记提取完成；
//! - `youtube_review_batch`：`YYYY-MM-DD` 一批到期词块，`/复习` 反馈或 3 天过期后 resolved；
//! - `youtube_posttest`：`<video>` 后测任务已入队标记（幂等键）。

use crate::{
    app::task::TaskService,
    bootstrap::assembly,
    domain::NotifyRoute,
    ingress::schedule::PollJob,
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::Result;
use chrono::Local;
use std::sync::Arc;

use super::service::YoutubeConfig;

/// 词块记录 kind；id 形如 `<video>#<n>`（n 为单张预习单内的序号）或 `<video>#meta`。
const VOCAB_KIND: &str = "youtube_vocab";
/// 每日复习批次 kind；id 为本地日期。
const BATCH_KIND: &str = "youtube_review_batch";
/// 后测幂等标记 kind；id 为 video_id。
const QUIZ_KIND: &str = "youtube_posttest";

/// 间隔阶梯（天）：每「会」进一档，走完最后一档毕业为 known。
pub const INTERVAL_DAYS: [i64; 5] = [1, 3, 7, 14, 30];
/// 复习单最多带几个词块，控制消息长度与记忆负荷。
const MAX_PER_PUSH: usize = 8;
/// 单批内同一个视频最多占几条——批里优先混入不同视频的词块。
const MAX_PER_VIDEO: usize = 2;
/// 批次未反馈多少小时后自动按「会」处理（避免无限重推）。
const BATCH_STALE_HOURS: i64 = 72;
/// 单轮扫描的任务数上限（约可覆盖半年的预习任务）。
const SCAN_LIMIT: u32 = 500;

/// 从预习单 summary 解析出的词块。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabItem {
    /// 英文词块（回忆的答案）。
    pub chunk: String,
    /// 中文情境线索（回忆的提示，不含英文原文）。
    pub cue: String,
    /// equip=现场装备（理解关键）；carry=带走就用（可迁移口语块）。
    pub kind: &'static str,
}

/// 复习任务：每日一跑（内部按日历日幂等），四步——提取、后测、过期批次结转、到期推送。
pub struct YoutubeReviewJob {
    store: Arc<dyn TaskStore>,
    config: YoutubeConfig,
    notifier: Option<Arc<dyn NotifyChannel>>,
}

impl YoutubeReviewJob {
    pub fn new(
        store: Arc<dyn TaskStore>,
        config: YoutubeConfig,
        notifier: Option<Arc<dyn NotifyChannel>>,
    ) -> Self {
        Self {
            store,
            config,
            notifier,
        }
    }

    async fn review_pass(&self) -> Result<serde_json::Value> {
        let today = Local::now().format("%Y-%m-%d").to_string();
        let mut extracted = 0;
        let mut quizzes = 0;
        let mut retired = 0;
        let mut pushed = 0;

        self.extract_new_vocab(&mut extracted)?;
        if self.config.notify.is_some() {
            quizzes = self.enqueue_posttests(today.clone()).await?;
            retired = self.retire_stale_batches()?;
            pushed = self.push_due_reviews(&today).await?;
        }

        Ok(serde_json::json!({
            "date": today,
            "extracted_videos": extracted,
            "posttests_enqueued": quizzes,
            "stale_batches_retired": retired,
            "review_pushed": pushed,
        }))
    }

    /// 对尚未提取过词块的已完成预习单做解析落库；幂等键为 `<video>#meta`。
    fn extract_new_vocab(&self, counter: &mut usize) -> Result<()> {
        let done_ids: Vec<String> = self
            .store
            .list(SCAN_LIMIT)?
            .into_iter()
            .filter(|task| {
                task.request_key.starts_with("youtube:prep:")
                    && task.status == crate::domain::TaskStatus::Succeeded
                    && task.result.as_ref().is_some_and(|r| !r.summary.is_empty())
            })
            .map(|task| task.request_key)
            .collect();
        for key in done_ids {
            let video = key
                .strip_prefix("youtube:prep:")
                .expect("prefix checked")
                .to_owned();
            if self
                .store
                .document(VOCAB_KIND, &format!("{video}#meta"))?
                .is_some()
            {
                continue;
            }
            let task = self
                .store
                .list(SCAN_LIMIT)?
                .into_iter()
                .find(|task| task.request_key == key);
            let Some(task) = task else { continue };
            let Some(result) = &task.result else { continue };
            let items = extract_vocab(&result.summary);
            let now = crate::util::now();
            for (index, item) in items.iter().enumerate() {
                self.store.put_document(
                    VOCAB_KIND,
                    &format!("{video}#{index}"),
                    None,
                    &serde_json::json!({
                        "video_id": video,
                        "chunk": item.chunk,
                        "cue": item.cue,
                        "kind": item.kind,
                        "learned_at": now,
                        "stage": 0,
                        "due_at": now + INTERVAL_DAYS[0] * 86_400,
                        "known": false,
                    }),
                )?;
            }
            self.store.put_document(
                VOCAB_KIND,
                &format!("{video}#meta"),
                None,
                &serde_json::json!({ "extracted": true, "count": items.len() }),
            )?;
            *counter += 1;
        }
        Ok(())
    }

    /// 看后测试：今天送达预习单的视频，入队一道后测任务（2 内容理解 + 1 语言运用）。
    /// 后测由 Agent 出题，走任务通知出口推送；幂等键 `youtube_posttest#<video>`。
    async fn enqueue_posttests(&self, today: String) -> Result<usize> {
        let Some(route) = self.config.notify.clone() else {
            return Ok(0);
        };
        let mut count = 0;
        for task in self.store.list(SCAN_LIMIT)? {
            if !task.request_key.starts_with("youtube:prep:")
                || task.status != crate::domain::TaskStatus::Succeeded
            {
                continue;
            }
            let video = task
                .request_key
                .strip_prefix("youtube:prep:")
                .expect("prefix checked")
                .to_owned();
            if self.store.document(QUIZ_KIND, &video)?.is_some() {
                continue;
            }
            let Some(notified) = &task.notified_at else {
                continue;
            };
            let Ok(at) = chrono::DateTime::parse_from_rfc3339(notified) else {
                continue;
            };
            if at.with_timezone(&Local).format("%Y-%m-%d").to_string() != today {
                continue;
            }
            let Some(result) = &task.result else { continue };
            let mut service = TaskService::new(self.store.clone());
            let key = format!("youtube:quiz:{video}");
            let enqueue = match &self.config.notify {
                Some(route) => {
                    service.enqueue_with_notify(&key, &quiz_prompt(&result.summary), &self.config.backend, route)?
                }
                None => service.enqueue_for(&key, &quiz_prompt(&result.summary), &self.config.backend)?,
            };
            self.store.put_document(
                QUIZ_KIND,
                &video,
                None,
                &serde_json::json!({ "task_id": enqueue.task.id, "date": today }),
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// 超过 72 小时未反馈的批次按「会」结转：进一档或毕业为 known。
    fn retire_stale_batches(&self) -> Result<usize> {
        let mut retired = 0;
        for doc in self.store.documents(BATCH_KIND, 60)? {
            if doc.value["resolved"].as_bool() == Some(true) {
                continue;
            }
            let pushed_at = doc.value["pushed_at"].as_i64().unwrap_or(0);
            if crate::util::now() - pushed_at < BATCH_STALE_HOURS * 3600 {
                continue;
            }
            if let Some(items) = doc.value["items"].as_array() {
                for item in items {
                    if let Some(id) = item.as_str() {
                        promote_item(&self.store, id)?;
                    }
                }
            }
            let mut value = doc.value.clone();
            value["resolved"] = true.into();
            value["resolved_as"] = "stale-会".into();
            self.store
                .put_document(BATCH_KIND, &doc.id, Some(doc.revision), &value)?;
            retired += 1;
        }
        Ok(retired)
    }

    /// 到期词块合成一条复习推送（含批次登记）；没有到期词块则不打扰。
    async fn push_due_reviews(&self, today: &str) -> Result<usize> {
        let Some(notifier) = &self.notifier else {
            return Ok(0);
        };
        let Some(route) = self.config.notify.clone() else {
            return Ok(0);
        };
        if self.store.document(BATCH_KIND, today)?.is_some() {
            return Ok(0);
        }
        let now = crate::util::now();
        let mut due: Vec<_> = self
            .store
            .documents(VOCAB_KIND, 2_000)?
            .into_iter()
            .filter(|doc| {
                !doc.id.ends_with("#meta")
                    && doc.value["known"].as_bool() != Some(true)
                    && doc.value["due_at"].as_i64().map(|d| d <= now).unwrap_or(false)
            })
            .collect();
        due.sort_by_key(|doc| doc.value["due_at"].as_i64().unwrap_or(i64::MAX));
        // 单批最多 MAX_PER_PUSH 条，且单个视频最多 MAX_PER_VIDEO 条——
        // 防止某一天只学了一个视频时，复习批被同一视频刷满。
        let mut picked: Vec<crate::domain::records::Document> = Vec::new();
        let mut per_video: std::collections::HashMap<String, usize> = Default::default();
        for doc in due {
            let video = doc.value["video_id"].as_str().unwrap_or_default().to_owned();
            let count = per_video.entry(video).or_insert(0);
            if *count >= MAX_PER_VIDEO {
                continue;
            }
            *count += 1;
            picked.push(doc);
            if picked.len() >= MAX_PER_PUSH {
                break;
            }
        }
        let due = picked;
        if due.is_empty() {
            return Ok(0);
        }

        let mut text = format!(
            "🔁 词块复习 {today}（先回忆再往下滑，答案在你之前的预习单里）\n回复 /复习 <不会的编号>（如 /复习 2 4）；全部都会回 /复习 all\n"
        );
        for (index, doc) in due.iter().enumerate() {
            let cue = doc.value["cue"].as_str().unwrap_or("").trim();
            let kind = doc.value["kind"].as_str().unwrap_or("装备");
            let tag = if kind == "carry" { "说" } else { "懂" };
            text.push_str(&format!("{}.（{tag}）{cue}\n", index + 1));
        }
        notifier.send(&route.target, None, &text).await?;
        let ids: Vec<&str> = due.iter().map(|doc| doc.id.as_str()).collect();
        self.store.put_document(
            BATCH_KIND,
            today,
            None,
            &serde_json::json!({ "items": ids, "pushed_at": now, "resolved": false }),
        )?;
        // 推送后给 2 天宽限，等反馈或过期结转，避免同一词块连日重推。
        for doc in &due {
            let mut value = doc.value.clone();
            value["due_at"] = (now + 2 * 86_400).into();
            let _ = self
                .store
                .put_document(VOCAB_KIND, &doc.id, Some(doc.revision), &value);
        }
        Ok(1)
    }
}

/// 「会」：进一档；走完阶梯毕业为 known（预习挑词将排除）。
fn promote_item(store: &Arc<dyn TaskStore>, id: &str) -> Result<()> {
    let Some(doc) = store.document(VOCAB_KIND, id)? else {
        return Ok(());
    };
    let mut value = doc.value.clone();
    if value["known"].as_bool() == Some(true) {
        return Ok(());
    }
    let stage = value["stage"].as_u64().unwrap_or(0) as usize;
    if stage + 1 >= INTERVAL_DAYS.len() {
        value["known"] = true.into();
    } else {
        let next = stage + 1;
        value["stage"] = next.into();
        value["due_at"] = (crate::util::now() + INTERVAL_DAYS[next] * 86_400).into();
    }
    store.put_document(VOCAB_KIND, id, Some(doc.revision), &value)?;
    Ok(())
}

/// 「不会」：回到第一档，明天重推。
fn demote_item(store: &Arc<dyn TaskStore>, id: &str) -> Result<()> {
    let Some(doc) = store.document(VOCAB_KIND, id)? else {
        return Ok(());
    };
    let mut value = doc.value.clone();
    value["stage"] = 0.into();
    value["known"] = false.into();
    value["due_at"] = (crate::util::now() + INTERVAL_DAYS[0] * 86_400).into();
    store.put_document(VOCAB_KIND, id, Some(doc.revision), &value)?;
    Ok(())
}

/// `/复习` 反馈入口：列出的编号记「不会」，其余（或 all 时全部）记「会」。
pub fn apply_feedback(store: &Arc<dyn TaskStore>, arg: &str) -> Result<String> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Ok(
            "用法：/复习 <不会的编号>（如 /复习 2 4，其余记会）；全部都会回 /复习 all".into(),
        );
    }
    let Some(batch) = latest_open_batch(store)? else {
        return Ok("最近 3 天没有待反馈的复习批次。".into());
    };
    let items: Vec<&str> = batch
        .value["items"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let fail_numbers = if arg == "all" {
        Vec::new()
    } else {
        match parse_numbers(arg, items.len()) {
            Ok(nums) => nums,
            Err(help) => return Ok(help),
        }
    };
    let mut known_count = 0;
    let mut failed = Vec::new();
    for (index, id) in items.iter().enumerate() {
        let number = index + 1;
        if fail_numbers.contains(&number) {
            demote_item(store, id)?;
            failed.push(number);
        } else {
            promote_item(store, id)?;
            known_count += 1;
        }
    }
    let mut value = batch.value.clone();
    value["resolved"] = true.into();
    value["resolved_as"] = format!("会 {known_count} / 不会 {:?}", failed).into();
    store.put_document(BATCH_KIND, &batch.id, Some(batch.revision), &value)?;
    if failed.is_empty() {
        Ok(format!("已记录：{known_count} 个都会，从复习名单毕业 🎉"))
    } else {
        Ok(format!("已记录：会 {known_count} 个，不会 {failed:?}——明天重推。"))
    }
}

fn latest_open_batch(
    store: &Arc<dyn TaskStore>,
) -> Result<Option<crate::domain::records::Document>> {
    let cutoff = crate::util::now() - BATCH_STALE_HOURS * 3600;
    Ok(store
        .documents(BATCH_KIND, 30)?
        .into_iter()
        .filter(|doc| {
            doc.value["resolved"].as_bool() != Some(true)
                && doc.value["pushed_at"].as_i64().unwrap_or(0) >= cutoff
        })
        .max_by_key(|doc| doc.value["pushed_at"].as_i64().unwrap_or(0)))
}

/// 已毕业（known）的词块表，喂给预习挑词做排除。
pub fn known_chunks(store: &Arc<dyn TaskStore>, cap: usize) -> Vec<String> {
    let mut chunks: Vec<String> = store
        .documents(VOCAB_KIND, 2_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|doc| doc.value["known"].as_bool() == Some(true))
        .filter_map(|doc| doc.value["chunk"].as_str().map(str::to_owned))
        .collect();
    chunks.sort();
    chunks.dedup();
    chunks.truncate(cap);
    chunks
}

/// `/复习 2, 4` / `/复习 2 4` → [2, 4]；越界或非数字返回错误提示。
fn parse_numbers(arg: &str, max: usize) -> Result<Vec<usize>, String> {
    let mut numbers = Vec::new();
    for token in arg.split([',', '，', ' ']).filter(|t| !t.is_empty()) {
        let Ok(number) = token.parse::<usize>() else {
            return Err(format!("看不懂「{token}」，用法：/复习 <不会的编号>，如 /复习 2 4"));
        };
        if number == 0 || number > max {
            return Err(format!("编号 {number} 超出范围（1-{max}）。"));
        }
        numbers.push(number);
    }
    if numbers.is_empty() {
        return Err("没有给出编号。用法：/复习 <不会的编号>，如 /复习 2 4".into());
    }
    Ok(numbers)
}

/// 从预习单 summary 提取词块：📖 现场装备 与 🔁 带走就用 两节。
/// 线索（cue）必须剥掉英文原句——那是复习的答案，不能出现在提示里。
pub fn extract_vocab(summary: &str) -> Vec<VocabItem> {
    let mut items = Vec::new();
    let mut section = "";
    for line in summary.lines() {
        let line = line.trim();
        if line.contains("现场装备") {
            section = "equip";
            continue;
        }
        if line.contains("带走就用") {
            section = "carry";
            continue;
        }
        if line.starts_with("👂") || line.starts_with("❓") || line.starts_with("📺") {
            section = "";
            continue;
        }
        if section.is_empty() || line.is_empty() || line.starts_with('（') {
            continue;
        }
        let body = match section {
            "equip" => line
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '.' || c == ' ')
                .strip_prefix("**"),
            "carry" => line.trim_start_matches("- ").strip_prefix("**\""),
            _ => None,
        };
        let Some(after_open) = body else { continue };
        let Some(close) = after_open.find("**") else { continue };
        let mut chunk = after_open[..close].to_owned();
        let mut cue = after_open[close + 2..].trim().to_owned();
        if section == "carry" {
            chunk = chunk.trim_matches('"').to_owned();
            if let Some(index) = cue.find("→ 你可以说") {
                cue = cue[..index].trim_end_matches(['；', ';', '—', '-']).to_owned();
            }
        }
        // 剥掉行尾英文原句：`；"..." [m:ss]`——提示里出现英文就泄题了。
        if let Some(index) = cue.rfind("；\"").or_else(|| cue.rfind("; \"")) {
            let tail = &cue[index..];
            if tail.contains("] ") || tail.ends_with(']') || tail.contains("\"]") {
                cue = cue[..index].trim_end_matches(['；', ';']).to_owned();
            }
        }
        if cue.is_empty() || chunk.is_empty() {
            continue;
        }
        items.push(VocabItem {
            chunk,
            cue,
            kind: if section == "carry" { "carry" } else { "equip" },
        });
    }
    items
}

/// 后测任务 prompt：基于预习单出题，Agent 只见单子不见视频——考的是「看懂了没」。
fn quiz_prompt(sheet: &str) -> String {
    format!(
        "你在为中国大学生（英语学习者）出一份「看视频后测」。下面是他看视频前读的预习单。\n\
         出 3 道题：2 道内容理解（这期视频讲了什么，预习单的路线图和词块是线索）、1 道语言运用（用预习单的词块造境选择或填空）。\n\
         输出格式（这就是最终推送的消息，不要输出格式以外的任何内容）：\n\
         ✅ 后测｜{{视频主题一句话}}\n\
         1. {{题}}\n2. {{题}}\n3. {{题}}\n\
         —— 参考答案 ——\n1. {{答案}}\n2. {{答案}}\n3. {{答案}}\n\
         要求：题目不让没看视频的人能答对；答案区与题目区之间空一行；全文 400 字以内。\n\n\
         == 预习单 ==\n{sheet}"
    )
}

impl PollJob for YoutubeReviewJob {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(6 * 3600)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move { self.review_pass().await })
    }
}

/// 装配入口：从配置解析通知渠道，复习推送与 batch 反馈共用同一路由。
pub fn review_job(
    store: Arc<dyn TaskStore>,
    config: &crate::bootstrap::config::Config,
    youtube: &YoutubeConfig,
) -> Result<YoutubeReviewJob> {
    let notifier = match &youtube.notify {
        Some(route) => Some(assembly::notifier_for_route(config, route)?),
        None => None,
    };
    Ok(YoutubeReviewJob::new(store, youtube.clone(), notifier))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "📺 工程师让 dot 助手收集反馈、排查故障。\n\
🔗 https://www.youtube.com/watch?v=6f7ojkd4_Wc\n⏱ 5:11\n\n\
🗺 听什么（按视频顺序 2-4 段，每段一行）：\n\
- [0:18] 建反馈页面 → 抓住：tracker 指反馈跟踪表\n\n\
📖 现场装备（4-6 个，宁少勿滥，只收理解这期视频的关键）：\n\
1. **silent stalls** 助手说在工作，却卡住不再回应；别误解为：正常安静运行；\"Okay. Silent stalls sounds like\" [2:44]\n\
2. **fall back** 首选模型不可用时改用可用模型；\"Instead, it could just fall back\" [3:32]\n\
（「别误解为」仅当该词块存在真实混淆点时才写，写在释义后；没有就整条省略，禁止凑数）\n\n\
🔁 带走就用（2-3 个可迁移口语块）：\n\
- **\"walk through … with you\"** 带人逐步看懂 → 你可以说：\"I'll walk through the code with you.\"；\"I'm going to walk through that with you.\" [2:02]\n\
- **\"Let me choose one of…\"** 从几个选项中挑一个 → 你可以说：\"Let me choose one of these topics.\"；\"Let me choose one of these issues.\" [0:54]\n\n\
👂 听力锚点（2-3 处，具体到时刻和音）：\n\
- [2:02] \"I'm going to walk through that with you.\" → 留意 going to 缩成 gonna。\n\n\
❓ 听前自测：为什么优先查 silent stalls？\n";

    #[test]
    fn extracts_chunks_without_leaking_english() {
        let items = extract_vocab(SAMPLE);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].chunk, "silent stalls");
        assert!(items[0].cue.contains("卡住"));
        assert!(!items[0].cue.contains("Silent stalls sounds like"), "原句泄漏: {}", items[0].cue);
        assert_eq!(items[0].kind, "equip");
        assert_eq!(items[2].chunk, "walk through … with you");
        assert_eq!(items[2].kind, "carry");
        assert!(
            !items[2].cue.contains("walk through the code"),
            "你可以说 例句泄漏: {}",
            items[2].cue
        );
        assert!(items[2].cue.contains("带人逐步看懂"));
    }

    #[test]
    fn feedback_numbers_parse_and_validate() {
        assert_eq!(parse_numbers("2 4", 5).unwrap(), vec![2, 4]);
        assert_eq!(parse_numbers("2，4", 5).unwrap(), vec![2, 4]);
        assert!(parse_numbers("abc", 5).is_err());
        assert!(parse_numbers("9", 5).is_err());
        assert!(parse_numbers("", 5).is_err());
    }
}
