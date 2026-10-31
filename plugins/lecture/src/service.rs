//! 转写 worker：单轮 poll 消费一条 pending 录音。
//! ffmpeg 切片（单声道 16k mp3）→ ASR 逐片转写 → Vault 存档 → 入队 agent 整理笔记。

use crate::asr::AsrClient;
use crate::config::LectureConfig;
use crate::pending::{sanitize_name, PendingItem, PENDING_KIND};
use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use ru41::app::schedule::ScheduleService;
use ru41::domain::todo::TodoOptions;
use ru41::ports::storage::TaskStore;
use ru41::ports::vault::Vault;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct LectureWorker {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    options: TodoOptions,
    config: LectureConfig,
}

impl LectureWorker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        vault: Arc<dyn Vault>,
        options: TodoOptions,
        config: LectureConfig,
    ) -> Self {
        Self {
            store,
            vault,
            options,
            config,
        }
    }

    /// 单轮：取最旧一条未完成项处理。run_periodic 串行等待，天然不并发。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let docs = self.store.documents(PENDING_KIND, 50)?;
        let doc = docs
            .iter()
            .filter(|d| {
                PendingItem::from_document(d)
                    .map(|i| i.status != "done" && i.status != "failed")
                    .unwrap_or(false)
            })
            .next()
            .cloned();
        let Some(doc) = doc else {
            return Ok(serde_json::json!({"processed": null}));
        };
        let item = PendingItem::from_document(&doc)?;
        let course = self.course_at(item.received_at);
        match self.process(&item, &course).await {
            Ok(transcript_path) => {
                self.update(&doc.revision, &item, |v| {
                    v.status = "transcribed".into();
                    v.transcript_path = Some(transcript_path.clone());
                    v.course = Some(course.clone());
                })?;
                // 已转写未入队（上次中断）也走这里：幂等 request_key 保证只入队一次。
                let enqueued = self.enqueue_agent(&item, &course, &transcript_path)?;
                self.update(&doc.revision, &item, |v| {
                    v.status = "done".into();
                    v.error = None;
                })?;
                Ok(serde_json::json!({
                    "processed": item.msg_id,
                    "course": course,
                    "transcript": transcript_path,
                    "task_id": enqueued.task.id,
                }))
            }
            Err(error) => {
                self.update(&doc.revision, &item, |v| {
                    v.status = "failed".into();
                    v.error = Some(format!("{error:#}"));
                })?;
                Ok(serde_json::json!({
                    "processed": item.msg_id,
                    "failed": format!("{error:#}"),
                }))
            }
        }
    }

    /// 转写管线：切片 → ASR → vault 存档。返回 vault 内转写路径。
    async fn process(&self, item: &PendingItem, course: &str) -> Result<String> {
        let work = self.config.workspace.join(&item.msg_id);
        let slices_dir = work.join("slices");
        std::fs::create_dir_all(&slices_dir)
            .with_context(|| format!("创建工作目录失败 {}", slices_dir.display()))?;
        let slices = self.ensure_slices(&work.join("raw").join(&item.filename), &slices_dir)?;
        anyhow::ensure!(
            !slices.is_empty(),
            "ffmpeg 未产出任何切片：{}",
            item.filename
        );
        let asr = AsrClient::new(
            &self.config.base_url,
            &self.config.api_key,
            &self.config.model,
        );
        let mut parts = Vec::new();
        for (index, slice) in slices.iter().enumerate() {
            let text = asr
                .transcribe(slice)
                .await
                .with_context(|| format!("转写第 {}/{} 片失败", index + 1, slices.len()))?;
            parts.push(text);
        }
        let transcript = parts.join("\n\n");
        let (date, stem) = archive_name(item);
        let relative = format!(
            "{}/{}/转写/{date} {stem}.md",
            self.config.vault_dir.trim_end_matches('/'),
            course
        );
        self.vault
            .write_file(&relative, &transcript_markdown(item, course, &date, &transcript))
            .with_context(|| format!("写入 Vault 失败：{relative}"))?;
        if let Err(error) = self.vault.commit_all(&format!("[lecture] 课堂录音转写 {course} {date}"))
        {
            eprintln!("[lecture] vault commit 失败（转写已在工作区，不影响）：{error:#}");
        }
        Ok(relative)
    }

    /// 原始文件切片；已有切片则跳过（中断续跑不重复转码）。
    fn ensure_slices(&self, raw: &Path, slices_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut existing: Vec<PathBuf> = std::fs::read_dir(slices_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
            })
            .collect();
        existing.sort();
        if !existing.is_empty() {
            return Ok(existing);
        }
        let pattern = slices_dir.join("slice%03d.mp3");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                &raw.display().to_string(),
                "-vn",
                "-ac",
                "1",
                "-ar",
                "16000",
                "-c:a",
                "libmp3lame",
                "-b:a",
                "64k",
                "-f",
                "segment",
                "-segment_time",
                &self.config.slice_secs.to_string(),
            ])
            .arg(&pattern)
            .output()
            .context("启动 ffmpeg 失败（镜像内应有 /usr/bin/ffmpeg）")?;
        anyhow::ensure!(
            output.status.success(),
            "ffmpeg 失败：{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut slices: Vec<PathBuf> = std::fs::read_dir(slices_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
            })
            .collect();
        slices.sort();
        Ok(slices)
    }

    /// 按收到录音的时刻猜课程：优先包含该时刻的课，否则 2 小时窗内最近的一节；
    /// 日程里没有匹配则回退 fallback_course。公开供集成测试。
    pub fn course_at(&self, received_at: i64) -> String {
        let window = 2 * 3600;
        let service = ScheduleService::new(self.store.clone(), self.options.clone());
        let Ok(occurrences) =
            service.occurrences_between(received_at - window, received_at + window, 20)
        else {
            return self.config.fallback_course.clone();
        };
        let containing = occurrences
            .iter()
            .find(|o| o.starts_at <= received_at && received_at < o.ends_at);
        let nearest = occurrences.iter().min_by_key(|o| {
            (received_at - o.starts_at).abs()
        });
        containing
            .or(nearest)
            .map(|o| sanitize_name(&o.title))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.config.fallback_course.clone())
    }

    /// 入队 agent 整理结构化笔记；request_key 幂等，重复 poll 不会重复建任务。
    fn enqueue_agent(
        &self,
        item: &PendingItem,
        course: &str,
        transcript_path: &str,
    ) -> Result<ru41::domain::EnqueueResult> {
        let (date, stem) = archive_name(item);
        let note_path = format!(
            "{}/{}/笔记/{date} {stem}.md",
            self.config.vault_dir.trim_end_matches('/'),
            course
        );
        let prompt = format!(
            "请把课堂录音转写整理成结构化课堂笔记。\n\
             - 课程：{course}\n\
             - 日期：{date}\n\
             - 转写原文（已在 Vault）：{transcript_path}\n\
             要求：\n\
             1. 读取转写原文，提炼整理为课堂笔记，写入 Vault：{note_path}\n\
             2. 笔记结构：## 本课概览、## 核心概念、## 详细笔记、## 重点与考点、## 复习清单。\n\
             3. 转写中的口语冗余、重复内容直接舍弃，专有名词与术语要准确。\n\
             4. 完成后简单汇报笔记要点（不超过 10 行），不要复述全文。",
        );
        self.store.enqueue_with_notify(
            &format!("lecture:notes:{}", item.msg_id),
            &prompt,
            "codex",
            ru41::domain::RunContract::AnalysisV1,
            &ru41::domain::NotifyRoute {
                channel: item.channel.clone(),
                target: item.target.clone(),
                msg_id: Some(item.msg_id.clone()),
            },
        )
    }

    /// 乐观并发更新 pending 文档；失败说明被并发改动，跳过本轮。
    fn update(
        &self,
        revision: &i64,
        item: &PendingItem,
        mutate: impl FnOnce(&mut PendingItem),
    ) -> Result<()> {
        let mut next = item.clone();
        mutate(&mut next);
        let written = self.store.put_document(
            PENDING_KIND,
            &item.msg_id,
            Some(*revision),
            &serde_json::to_value(&next)?,
        )?;
        anyhow::ensure!(written, "lecture.pending {} 状态更新竞争失败", item.msg_id);
        Ok(())
    }
}

impl ru41::ingress::schedule::PollJob for LectureWorker {
    fn job_interval(&self) -> Duration {
        Duration::from_secs(crate::config::LectureSection::DEFAULT_INTERVAL_SECS)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<serde_json::Value>> + Send + '_>> {
        Box::pin(async { self.poll_once().await })
    }
}

/// 归档命名：日期取本地时区收到录音的日期；stem 来自原始文件名。
fn archive_name(item: &PendingItem) -> (String, String) {
    let date = Local
        .timestamp_opt(item.received_at, 0)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let stem = Path::new(&item.filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_name)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("课堂录音 {}", item.msg_id));
    (date, stem)
}

fn transcript_markdown(item: &PendingItem, course: &str, date: &str, transcript: &str) -> String {
    format!(
        "---\ncourse: {course}\ndate: {date}\ntype: 转写\nsource: {}\n---\n\n\
         # 课堂录音转写（{date} · {course}）\n\n\
         > 原始文件：{}（{:.1} MB）；本文件为 ASR 原文，结构化笔记见同课程「笔记」目录。\n\n{transcript}\n",
        item.filename,
        item.filename,
        item.size as f64 / 1024.0 / 1024.0,
    )
}
