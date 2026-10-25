//! 复习闭环：预习单词块落库 → 每日间隔复习推送 → 看后测试 → 已会词反哺预习挑词。
//!
//! 预习单只解决「看懂」，学习发生在看之后：本文件实现遗忘对抗的机制——
//! 词块按 1/3/7/14/30 天间隔推 QQ 回忆题（间隔阶梯）；看视频次日推 Agent 后测题；
//! 「会」过的词进已知名单，预习挑词自动排除；听力锚点按语音现象归类攒专题，
//! 攒够即推发音专题课（例句全是看过的原句）；`/定级` 三句翻译测出 CEFR 级别，
//! 反哺预习挑词难度。
//!
//! records 表数据模型：
//! - `youtube_vocab`：`<video>#<n>` 一词一条（chunk/cue/stage/due_at/known），`<video>#meta` 标记提取完成；
//! - `youtube_review_batch`：`YYYY-MM-DD` 一批到期词块，`/复习` 反馈或 3 天过期后 resolved；
//! - `youtube_posttest`：`<video>` 后测任务已入队标记（幂等键）；
//! - `youtube_anchor`：`<video>#a<n>` 听力锚点（ts/quote/note/category/topic_pushed）；
//! - `youtube_placement`：`latest` 定级测试状态（questions→answers）；
//! - `youtube_level`：`latest` 定级结果（level/task_id），预习 prompt 的级别来源。

use crate::{
    app::task::TaskService,
    bootstrap::assembly,
    domain::NotifyRoute,
    ingress::schedule::PollJob,
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use anyhow::Result;
use chrono::{Local, Timelike};
use std::sync::Arc;

use super::service::YoutubeConfig;

/// 词块记录 kind；id 形如 `<video>#<n>`（n 为单张预习单内的序号）或 `<video>#meta`。
const VOCAB_KIND: &str = "youtube_vocab";
/// 每日复习批次 kind；id 为本地日期。
const BATCH_KIND: &str = "youtube_review_batch";
/// 后测幂等标记 kind；id 为 video_id。
/// 听力锚点记录 kind；id 形如 `<video>#a<n>`。
const ANCHOR_KIND: &str = "youtube_anchor";
/// 发音专题「当日已推」标记 kind；id 固定 `latest`。
const TOPIC_KIND: &str = "youtube_anchor_topic";
/// 每日日报「当日已发」标记 kind；id 为本地日期。
const DIGEST_KIND: &str = "youtube_digest";
/// 日报窗口宽度（分钟）：job 15 分钟一跳，保证窗口内必有一次触发。
const DIGEST_WINDOW_MINS: i64 = 15;
/// 定级测试状态 kind；id 固定 `latest`。
const PLACEMENT_KIND: &str = "youtube_placement";
/// 定级结果 kind；id 固定 `latest`。
const LEVEL_KIND: &str = "youtube_level";

/// 间隔阶梯（天）：每「会」进一档，走完最后一档毕业为 known。
pub const INTERVAL_DAYS: [i64; 5] = [1, 3, 7, 14, 30];
/// 复习单最多带几个词块，控制消息长度与记忆负荷。
const MAX_PER_PUSH: usize = 8;
/// 单批内同一个视频最多占几条——批里优先混入不同视频的词块。
const MAX_PER_VIDEO: usize = 2;
/// 单期日报最多带的预习单数（控制消息长度）。
const MAX_SHEETS_PER_DIGEST: usize = 2;

/// 解析 "HH:MM" 为 (时, 分)；非法输入返回 None（走默认值）。
fn parse_hhmm(s: &str) -> Option<(i64, i64)> {
    let (h, m) = s.split_once(':')?;
    let h = h.trim().parse::<i64>().ok()?;
    let m = m.trim().parse::<i64>().ok()?;
    (h < 24 && m < 60).then_some((h, m))
}
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

/// 从预习单 summary 解析出的听力锚点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorItem {
    /// 时间戳，如 "2:02"。
    pub ts: String,
    /// 英文原句（逐字来自转写）。
    pub quote: String,
    /// 具体音变描述。
    pub note: String,
    /// 语音现象分类（连读/弱读/闪音/失爆/同化/缩读/重音/其他）。
    pub category: String,
}

/// 听力锚点的现象分类全集；预习 prompt 中的现象标签必须出自这里。
pub const ANCHOR_CATEGORIES: [&str; 8] = [
    "连读", "弱读", "闪音", "失爆", "同化", "缩读", "重音", "其他",
];

/// 发音专题推送需要的同现象新锚点数。
const TOPIC_MIN_ANCHORS: usize = 3;
/// 专题单次最多带几个例句。
const TOPIC_MAX_EXAMPLES: usize = 4;

/// 复习任务：每日一跑（内部按日历日幂等），四步——提取、后测、过期批次结转、到期推送。
pub struct YoutubeReviewJob {
    store: Arc<dyn TaskStore>,
    config: YoutubeConfig,
    notifier: Option<Arc<dyn NotifyChannel>>,
    /// 时钟注入点（测试固定时刻）；生产用本地时钟。
    now_fn: Arc<dyn Fn() -> chrono::DateTime<chrono::Local> + Send + Sync>,
}

impl YoutubeReviewJob {
    pub fn new(
        store: Arc<dyn TaskStore>,
        config: YoutubeConfig,
        notifier: Option<Arc<dyn NotifyChannel>>,
    ) -> Self {
        Self::with_clock(store, config, notifier, Arc::new(chrono::Local::now))
    }

    pub fn with_clock(
        store: Arc<dyn TaskStore>,
        config: YoutubeConfig,
        notifier: Option<Arc<dyn NotifyChannel>>,
        now_fn: Arc<dyn Fn() -> chrono::DateTime<chrono::Local> + Send + Sync>,
    ) -> Self {
        Self {
            store,
            config,
            notifier,
            now_fn,
        }
    }

    async fn review_pass(&self) -> Result<serde_json::Value> {
        let now = (self.now_fn)();
        let today = now.format("%Y-%m-%d").to_string();
        let mut extracted = 0;
        let mut retired = 0;
        let mut digest_pushed = 0;

        self.extract_new_vocab(&mut extracted)?;
        if self.config.notify.is_some() {
            retired = self.retire_stale_batches()?;
            self.pickup_level()?;
            // 日报窗口内且当天未发过：新预习单 + 到期词块 + 发音专题，一条打包。
            if self.in_digest_window(&now) && self.store.document(DIGEST_KIND, &today)?.is_none() {
                digest_pushed = self.push_daily_digest(&today).await?;
            }
        }

        Ok(serde_json::json!({
            "date": today,
            "extracted_videos": extracted,
            "stale_batches_retired": retired,
            "digest_pushed": digest_pushed,
        }))
    }

    /// 当前时刻是否落在日报窗口 [digest_at, digest_at + DIGEST_WINDOW_MINS) 内。
    fn in_digest_window(&self, now: &chrono::DateTime<chrono::Local>) -> bool {
        let Some((hour, minute)) = parse_hhmm(&self.config.digest_at) else {
            return false;
        };
        let start = hour * 60 + minute;
        let now_mins = now.hour() as i64 * 60 + now.minute() as i64;
        now_mins >= start && now_mins < start + DIGEST_WINDOW_MINS
    }

    /// 每日日报：所有待推送内容合成一条消息；发送成功后才落各自的副作用记录。
    async fn push_daily_digest(&self, today: &str) -> Result<usize> {
        let Some(notifier) = &self.notifier else {
            return Ok(0);
        };
        let Some(route) = self.config.notify.clone() else {
            return Ok(0);
        };
        let mmdd = &today[5..];
        let mut text = format!("📚 英语日报 {mmdd}\n");
        let mut has_content = false;

        // 新预习单（自动轮询发现的视频；手动 /预习 早已即时推送，不重复）。
        let sheet_ids = self.pending_sheet_ids()?;
        let sheets: Vec<_> = sheet_ids
            .iter()
            .filter_map(|id| self.store.get(*id).ok().flatten())
            .collect();
        for task in sheets.iter() {
            if let Some(result) = &task.result {
                text.push_str(&format!("\n───\n{}\n", result.summary.trim()));
                has_content = true;
            }
        }

        // 到期词块复习。
        let mut batch_ids = None;
        if let Some((section, ids)) = self.review_section()? {
            text.push_str(&section);
            batch_ids = Some(ids);
            has_content = true;
        }

        // 发音专题（每类攒够 TOPIC_MIN_ANCHORS 条，日历日至多一期）。
        let mut topic_docs = None;
        if let Some(doc) = self.store.document(TOPIC_KIND, "latest")?
            && doc.value["date"].as_str() == Some(today)
        {
            // 今天已推过专题，不再附第二期。
        } else if let Ok(Some((section, docs))) = self.topic_section() {
            text.push_str(&section);
            topic_docs = Some(docs);
            has_content = true;
        }

        if !has_content {
            return Ok(0);
        }
        notifier.send(&route.target, None, &text).await?;

        // 副作用：日报标记、批次登记与宽限、专题标记。
        self.store.put_document(
            DIGEST_KIND,
            today,
            None,
            &serde_json::json!({ "task_ids": sheet_ids, "pushed_at": crate::util::now() }),
        )?;
        if let Some(ids) = batch_ids {
            self.apply_batch_effects(&ids)?;
        }
        if let Some(docs) = topic_docs {
            self.apply_topic_effects(today, docs)?;
        }
        Ok(1)
    }

    /// 待进日报的预习单任务 id：自动轮询（无通知路由）且从未上过日报。
    fn pending_sheet_ids(&self) -> Result<Vec<i64>> {
        let mut digested: std::collections::HashSet<i64> = Default::default();
        for doc in self.store.documents(DIGEST_KIND, 60)? {
            if let Some(ids) = doc.value["task_ids"].as_array() {
                digested.extend(ids.iter().filter_map(|v| v.as_i64()));
            }
        }
        Ok(self
            .store
            .list(SCAN_LIMIT)?
            .into_iter()
            .filter(|task| {
                task.request_key.starts_with("youtube:prep:")
                    && task.status == crate::domain::TaskStatus::Succeeded
                    && task.notify.is_none()
                    && task.result.as_ref().is_some_and(|r| !r.summary.is_empty())
                    && !digested.contains(&task.id)
            })
            .take(MAX_SHEETS_PER_DIGEST)
            .map(|task| task.id)
            .collect())
    }

    /// 到期词块复习段：只负责挑词与排版；副作用由 push_daily_digest 统一落。
    fn review_section(&self) -> Result<Option<(String, Vec<String>)>> {
        let now = crate::util::now();
        let mut due: Vec<_> = self
            .store
            .documents(VOCAB_KIND, 2_000)?
            .into_iter()
            .filter(|doc| {
                !doc.id.ends_with("#meta")
                    && doc.value["known"].as_bool() != Some(true)
                    && doc.value["due_at"]
                        .as_i64()
                        .map(|d| d <= now)
                        .unwrap_or(false)
            })
            .collect();
        due.sort_by_key(|doc| doc.value["due_at"].as_i64().unwrap_or(i64::MAX));
        // 单批最多 MAX_PER_PUSH 条，且单个视频最多 MAX_PER_VIDEO 条——
        // 防止某一天只学了一个视频时，复习批被同一视频刷满。
        let mut picked: Vec<crate::domain::records::Document> = Vec::new();
        let mut per_video: std::collections::HashMap<String, usize> = Default::default();
        for doc in due {
            let video = doc.value["video_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
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
        if picked.is_empty() {
            return Ok(None);
        }
        let mut section = format!(
            "\n🔁 词块复习 {} 条（先回忆再往下滑；回复 /复习 <不会的编号>，如 /复习 2 4；全部都会回 /复习 all）\n",
            picked.len()
        );
        for (index, doc) in picked.iter().enumerate() {
            let cue = doc.value["cue"].as_str().unwrap_or("").trim();
            let kind = doc.value["kind"].as_str().unwrap_or("装备");
            let tag = if kind == "carry" { "说" } else { "懂" };
            section.push_str(&format!("{}.（{tag}）{cue}\n", index + 1));
        }
        let ids = picked.iter().map(|doc| doc.id.clone()).collect();
        Ok(Some((section, ids)))
    }

    /// 批次登记 + 2 天宽限：等反馈或过期结转，避免同一词块连日重推。
    fn apply_batch_effects(&self, ids: &[String]) -> Result<()> {
        let today = (self.now_fn)().format("%Y-%m-%d").to_string();
        self.store.put_document(
            BATCH_KIND,
            &today,
            None,
            &serde_json::json!({ "items": ids, "pushed_at": crate::util::now(), "resolved": false }),
        )?;
        for id in ids {
            if let Some(doc) = self.store.document(VOCAB_KIND, id)? {
                let mut value = doc.value.clone();
                value["due_at"] = (crate::util::now() + 2 * 86_400).into();
                let _ = self
                    .store
                    .put_document(VOCAB_KIND, id, Some(doc.revision), &value);
            }
        }
        Ok(())
    }

    /// 发音专题段：同一现象的新锚点攒够 TOPIC_MIN_ANCHORS 条，取最新几条做例句。
    fn topic_section(&self) -> Result<Option<(String, Vec<crate::domain::records::Document>)>> {
        let mut by_category: std::collections::HashMap<
            String,
            Vec<crate::domain::records::Document>,
        > = Default::default();
        for doc in self.store.documents(ANCHOR_KIND, 2_000)? {
            if doc.value["topic_pushed"].as_bool() == Some(true) {
                continue;
            }
            let category = doc.value["category"].as_str().unwrap_or("其他").to_owned();
            by_category.entry(category).or_default().push(doc);
        }
        let Some((category, mut docs)) = by_category
            .into_iter()
            .filter(|(_, docs)| docs.len() >= TOPIC_MIN_ANCHORS)
            .max_by_key(|(_, docs)| docs.len())
        else {
            return Ok(None);
        };
        docs.sort_by_key(|doc| doc.value["created_at"].as_i64().unwrap_or(0));
        let examples: Vec<_> = docs.iter().rev().take(TOPIC_MAX_EXAMPLES).collect();
        let mut section = format!(
            "\n🔊 发音专题｜{category}（{} 例，均来自你看过的视频；先回看对应时刻，再跟读一遍）\n",
            examples.len()
        );
        for (index, doc) in examples.iter().enumerate() {
            let ts = doc.value["ts"].as_str().unwrap_or("");
            let quote = doc.value["quote"].as_str().unwrap_or("");
            let note = doc.value["note"].as_str().unwrap_or("");
            section.push_str(&format!("{}. [{ts}] \"{quote}\" — {note}\n", index + 1));
        }
        Ok(Some((section, docs)))
    }

    /// 专题副作用：标记 topic_pushed + 记录当日已推。
    fn apply_topic_effects(
        &self,
        today: &str,
        docs: Vec<crate::domain::records::Document>,
    ) -> Result<()> {
        let category = docs
            .first()
            .map(|doc| doc.value["category"].as_str().unwrap_or("其他").to_owned());
        for doc in &docs {
            let mut value = doc.value.clone();
            value["topic_pushed"] = true.into();
            let _ = self
                .store
                .put_document(ANCHOR_KIND, &doc.id, Some(doc.revision), &value);
        }
        upsert_latest(
            &self.store,
            TOPIC_KIND,
            &serde_json::json!({ "date": today, "category": category }),
        )?;
        Ok(())
    }

    /// 定级结果回收：最新的已完成评测任务里解析 `定级：<CEFR>`，写入 youtube_level。
    /// 预习挑词从此用它替代配置里的静态 level。
    fn pickup_level(&self) -> Result<()> {
        let Some(task) = self.store.list(SCAN_LIMIT)?.into_iter().find(|task| {
            task.request_key.starts_with("youtube:place-eval:")
                && task.status == crate::domain::TaskStatus::Succeeded
                && task.result.as_ref().is_some_and(|r| !r.summary.is_empty())
        }) else {
            return Ok(());
        };
        if let Some(doc) = self.store.document(LEVEL_KIND, "latest")?
            && doc.value["task_id"].as_i64() == Some(task.id)
        {
            return Ok(());
        }
        let Some(summary) = task.result.as_ref().map(|r| r.summary.clone()) else {
            return Ok(());
        };
        let Some(level) = parse_level(&summary) else {
            return Ok(());
        };
        let value = serde_json::json!({
            "level": level,
            "task_id": task.id,
            "date": crate::util::now(),
        });
        match self.store.document(LEVEL_KIND, "latest")? {
            Some(doc) => {
                let _ = self
                    .store
                    .put_document(LEVEL_KIND, "latest", Some(doc.revision), &value);
            }
            None => {
                self.store
                    .put_document(LEVEL_KIND, "latest", None, &value)?;
            }
        }
        Ok(())
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
            let anchors = extract_anchors(&result.summary);
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
            for (index, anchor) in anchors.iter().enumerate() {
                self.store.put_document(
                    ANCHOR_KIND,
                    &format!("{video}#a{index}"),
                    None,
                    &serde_json::json!({
                        "video_id": video,
                        "ts": anchor.ts,
                        "quote": anchor.quote,
                        "note": anchor.note,
                        "category": anchor.category,
                        "created_at": now,
                        "topic_pushed": false,
                    }),
                )?;
            }
            self.store.put_document(
                VOCAB_KIND,
                &format!("{video}#meta"),
                None,
                &serde_json::json!({ "extracted": true, "count": items.len(), "anchors": anchors.len() }),
            )?;
            *counter += 1;
        }
        Ok(())
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
    let items: Vec<&str> = batch.value["items"]
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
        Ok(format!(
            "已记录：会 {known_count} 个，不会 {failed:?}——明天重推。"
        ))
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
            return Err(format!(
                "看不懂「{token}」，用法：/复习 <不会的编号>，如 /复习 2 4"
            ));
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
        let Some(close) = after_open.find("**") else {
            continue;
        };
        let mut chunk = after_open[..close].to_owned();
        let mut cue = after_open[close + 2..].trim().to_owned();
        if section == "carry" {
            chunk = chunk.trim_matches('"').to_owned();
            if let Some(index) = cue.find("→ 你可以说") {
                cue = cue[..index]
                    .trim_end_matches(['；', ';', '—', '-'])
                    .to_owned();
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

/// 从预习单 summary 提取听力锚点（👂 节）：`- [m:ss] "原句" → 音变描述（现象：X）`。
/// 现象标签缺失时归入「其他」。
pub fn extract_anchors(summary: &str) -> Vec<AnchorItem> {
    let mut items = Vec::new();
    let mut in_section = false;
    for line in summary.lines() {
        let line = line.trim();
        if line.starts_with("👂") {
            in_section = true;
            continue;
        }
        if line.starts_with("❓") {
            in_section = false;
            continue;
        }
        if !in_section || line.is_empty() {
            continue;
        }
        let Some(body) = line.strip_prefix("- ") else {
            continue;
        };
        // [m:ss] "quote" → note（现象：X）
        let Some(ts_end) = body.find(']') else {
            continue;
        };
        let ts = body[1..ts_end].trim().to_owned();
        if !ts.contains(':') {
            continue;
        }
        let rest = body[ts_end + 1..].trim_start();
        let Some(after_open) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(close_rel) = after_open.find('"') else {
            continue;
        };
        let quote = after_open[..close_rel].to_owned();
        let after_quote = after_open[close_rel + 1..].trim_start();
        let Some(after_arrow) = after_quote.strip_prefix("→") else {
            continue;
        };
        let mut note = after_arrow.trim().to_owned();
        let mut category = "其他".to_owned();
        if let Some(start) = note.find("（现象：") {
            let tail = &note[start + "（现象：".len()..];
            if let Some(end) = tail.find('）') {
                let tag = tail[..end].trim().to_owned();
                if ANCHOR_CATEGORIES.contains(&tag.as_str()) {
                    category = tag;
                }
                note = note[..start].trim_end().to_owned();
            }
        }
        if quote.is_empty() || note.is_empty() {
            continue;
        }
        items.push(AnchorItem {
            ts,
            quote,
            note,
            category,
        });
    }
    items
}

/// 从评测任务输出首行解析定级（如 `🔊 定级：B2+`）。
fn parse_level(summary: &str) -> Option<String> {
    let first = summary.lines().next()?.trim();
    let index = first.find("定级：")? + "定级：".len();
    let level = first[index..].trim();
    let valid = level.len() == 2 || level.len() == 3;
    let bytes = level.as_bytes();
    let ok = valid
        && matches!(bytes[0], b'A' | b'B' | b'C')
        && bytes[1].is_ascii_digit()
        && (level.len() == 2 || bytes[2] == b'+');
    ok.then(|| level.to_owned())
}

/// 预习挑词用的学习者级别：定级结果优先，配置兜底。
pub fn current_level(store: &Arc<dyn TaskStore>, fallback: &str) -> String {
    store
        .document(LEVEL_KIND, "latest")
        .ok()
        .flatten()
        .and_then(|doc| doc.value["level"].as_str().map(str::to_owned))
        .unwrap_or_else(|| fallback.to_owned())
}

/// `/定级`：出 3 道英译中测试题（取材最近一期预习单的视频），走任务推送。
pub async fn place_start(
    store: &Arc<dyn TaskStore>,
    backend: &str,
    route: Option<&NotifyRoute>,
) -> Result<String> {
    let today = Local::now().format("%Y%m%d").to_string();
    let key = format!("youtube:place:{today}");
    let sheet = latest_prep_summary(store)?;
    let prompt = placement_prompt(sheet.as_deref());
    let mut service = TaskService::new(store.clone());
    let enqueue = enqueue_with_route(&mut service, &key, &prompt, backend, route)?;
    let value = serde_json::json!({
        "stage": "questions",
        "task_id": enqueue.task.id,
        "date": today,
    });
    upsert_latest(store, PLACEMENT_KIND, &value)?;
    Ok("定级测试已生成，稍后推送。收到 3 个句子后，把你的翻译发来：/定级 <三句翻译>".into())
}

/// `/定级 <翻译>`：把用户翻译与原题一起交给 Agent 评测，产出级别写入 youtube_level。
pub async fn place_submit(
    store: &Arc<dyn TaskStore>,
    translations: &str,
    backend: &str,
    route: Option<&NotifyRoute>,
) -> Result<String> {
    let Some(doc) = store.document(PLACEMENT_KIND, "latest")? else {
        return Ok("还没有定级测试。先发 /定级 获取 3 个句子。".into());
    };
    if doc.value["stage"].as_str() != Some("questions") {
        return Ok("最近一次定级已经交卷了。再发一次 /定级 可重新测试。".into());
    }
    let Some(task_id) = doc.value["task_id"].as_i64() else {
        return Ok("定级测试状态异常，请重新发 /定级。".into());
    };
    let Some(task) = store.get(task_id)? else {
        return Ok("找不到定级测试题目，请重新发 /定级。".into());
    };
    let Some(questions) = task.result.as_ref().map(|r| r.summary.clone()) else {
        return Ok("定级测试还没生成完，稍等片刻再回复翻译。".into());
    };
    let today = doc.value["date"].as_str().unwrap_or("na").to_owned();
    let key = format!("youtube:place-eval:{today}");
    let prompt = eval_prompt(&questions, translations);
    let mut service = TaskService::new(store.clone());
    let enqueue = enqueue_with_route(&mut service, &key, &prompt, backend, route)?;
    let value = serde_json::json!({
        "stage": "answers",
        "task_id": enqueue.task.id,
        "date": today,
    });
    upsert_latest(store, PLACEMENT_KIND, &value)?;
    Ok("收到，评测中——结果稍后推送。".into())
}

/// 最新一期已完成预习单的 summary，给出题提供风格素材。
fn latest_prep_summary(store: &Arc<dyn TaskStore>) -> Result<Option<String>> {
    Ok(store
        .list(SCAN_LIMIT)?
        .into_iter()
        .find(|task| {
            task.request_key.starts_with("youtube:prep:")
                && task.status == crate::domain::TaskStatus::Succeeded
                && task.result.as_ref().is_some_and(|r| !r.summary.is_empty())
        })
        .and_then(|task| task.result.map(|r| r.summary)))
}

/// CAS 覆盖 `latest` 单例记录。
fn upsert_latest(store: &Arc<dyn TaskStore>, kind: &str, value: &serde_json::Value) -> Result<()> {
    match store.document(kind, "latest")? {
        Some(doc) => {
            store.put_document(kind, "latest", Some(doc.revision), value)?;
        }
        None => {
            store.put_document(kind, "latest", None, value)?;
        }
    }
    Ok(())
}

fn enqueue_with_route(
    service: &mut TaskService,
    key: &str,
    prompt: &str,
    backend: &str,
    route: Option<&NotifyRoute>,
) -> Result<crate::domain::EnqueueResult> {
    match route {
        Some(route) => service.enqueue_with_notify(key, prompt, backend, route),
        None => service.enqueue_for(key, prompt, backend),
    }
}

/// 定级出题 prompt：输出即推送文本。
fn placement_prompt(sheet: Option<&str>) -> String {
    let context = match sheet {
        Some(sheet) => {
            format!("参考素材（用户最近看的一期视频的预习单，句子风格尽量贴近它）：\n{sheet}\n\n")
        }
        None => String::new(),
    };
    format!(
        "你在给一位中国大学生（英语学习者）出英语定级测试。{context}\
         出 3 道英译中题：3 个英文句子，难度从易到难（大致对应 A2→B1→B2+），\
         优先取材参考素材里的原句或其风格；禁止用太难的专业术语堆砌难度。\
         输出就是最终推送的消息，格式：\n\
         📏 定级测试｜把你的翻译发来：/定级 <三句翻译>（不认识的词可保留英文）\n\
         1. {{句子1}}\n2. {{句子2}}\n3. {{句子3}}\n\
         全文 250 字以内，不要输出其他内容。"
    )
}

/// 定级评测 prompt：首行的 `🔊 定级：<级别>` 会被系统解析为学习者级别。
fn eval_prompt(questions: &str, translations: &str) -> String {
    format!(
        "你是有经验的英语教师。下面是给学生的 3 道英译中定级题和他的翻译。{questions}\n\n\
         == 学生翻译 ==\n{translations}\n\n\
         评测要求：\n\
         第一行必须是：🔊 定级：<级别>（从 A1/A2/B1/B2/C1/C2 及 + 档中判一个，如 B2+）\n\
         然后逐句点评：对/偏差/错在哪，怎么改更好，各一句；最后一条建议：这个级别最该练什么。\n\
         全文 300 字以内，语气直接，不说客套话。"
    )
}

impl PollJob for YoutubeReviewJob {
    /// 15 分钟一跳；日报窗口 15 分钟宽，保证每天必有一次落在窗口内。
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(15 * 60)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
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
        assert!(
            !items[0].cue.contains("Silent stalls sounds like"),
            "原句泄漏: {}",
            items[0].cue
        );
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
