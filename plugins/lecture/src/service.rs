//! 转写 worker：单轮 poll 消费一条 pending 录音。
//! ffmpeg 切片（单声道 16k mp3）→ ASR 逐片转写 → 转写内联进 agent prompt
//! → agent 把结构化笔记写进 Vault。vault 里只沉淀笔记；转写与音频都是临时产物。

use crate::asr::AsrClient;
use crate::config::LectureConfig;
use crate::pending::{sanitize_name, PendingItem, PENDING_KIND};
use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use ru41::app::schedule::ScheduleService;
use ru41::domain::todo::TodoOptions;
use ru41::ports::storage::TaskStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub struct LectureWorker {
    store: Arc<dyn TaskStore>,
    options: TodoOptions,
    config: LectureConfig,
}

impl Clone for LectureWorker {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            options: self.options,
            config: self.config.clone(),
        }
    }
}

impl LectureWorker {
    pub fn new(store: Arc<dyn TaskStore>, options: TodoOptions, config: LectureConfig) -> Self {
        Self {
            store,
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
        match self.transcribe(&item).await {
            Ok(transcript) => {
                // 转写只存在于内存与 agent prompt，不落 vault。
                let enqueued = self.enqueue_agent(&item, &course, &transcript)?;
                self.update(&doc.revision, &item, |v| {
                    v.status = "done".into();
                    v.course = Some(course.clone());
                    v.error = None;
                })?;
                // 入队成功后立即清理：转写已在 prompt 里，音频素材已无用。
                // 一节课的原始文件+切片几十 MB，不清理一学期吃十几个 GB。
                let work = self.config.workspace.join(&item.msg_id);
                if let Err(error) = std::fs::remove_dir_all(&work) {
                    eprintln!("[lecture] 清理工作目录失败（不影响流程）：{error:#}");
                }
                Ok(serde_json::json!({
                    "processed": item.msg_id,
                    "course": course,
                    "transcript_chars": transcript.chars().count(),
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

    /// 切片 → ASR 逐片转写拼接。中断重跑时切片已有产物直接复用，
    /// 转写本身不落盘（失败重投会重调 ASR，费用可控）。
    async fn transcribe(&self, item: &PendingItem) -> Result<String> {
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
            &self.config.api,
        )?;
        let mut parts = Vec::new();
        for (index, slice) in slices.iter().enumerate() {
            let text = asr
                .transcribe(slice)
                .await
                .with_context(|| format!("转写第 {}/{} 片失败", index + 1, slices.len()))?;
            parts.push(text);
        }
        Ok(parts.join("\n\n"))
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
        let service = ScheduleService::new(self.store.clone(), self.options);
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

    /// 入队 agent 整理结构化笔记；转写全量内联进 prompt，agent 只写笔记进 Vault。
    /// 一门课一份连续笔记（课堂笔记.md），每次课按日期章节追加在末尾。
    /// request_key 幂等，重复 poll 不会重复建任务。
    fn enqueue_agent(
        &self,
        item: &PendingItem,
        course: &str,
        transcript: &str,
    ) -> Result<ru41::domain::EnqueueResult> {
        let (date, stem) = archive_name(item);
        let note_path = format!(
            "{}/{}/课堂笔记.md",
            self.config.vault_dir.trim_end_matches('/'),
            course
        );
        let prompt = format!(
            "请把课堂录音转写整理成课堂笔记，用途只有一个：期末复习拿分。宁可短而实，不要长而空。\n\
             - 课表猜测课程：{course}（按收到录音的时刻查课表得出，课表可能有调课/错位，仅供参考）\n\
             - 日期：{date}\n\
             - 目标文件（Vault）：{note_path}\n\
             要求：\n\
             0. **先核实课程**：读一遍转写内容判断实际是哪门课，列一下 {vault_dir}/ 下现有课程目录名做对照。\
             若内容与猜测课程明显不符，改用你判断的课程，目标文件变为对应课程目录下的 课堂笔记.md（目录不存在才新建），\
             并在汇报里说明课程修正；内容无法判断时保持原目标。\
             注意有些课程目录名带额外信息（如「高等数学I(理)」），以目录名为准，不要自造新名字。\n\
             1. 目标文件不存在就创建（带 # <课程名> 课堂笔记 标题）；已存在就读取后**在末尾追加**新章节，已有内容一字不动。\n\
             2. 新章节格式：`## {date} {stem}`。栏目按需取用，没有实质内容的栏目**整个省略不写**（不留空标题、不写「无」）：\
             ### 本课概览（2-4 条说清这节课实际讲了什么）、### 例题与讲解、### 题型与方法、### 公式与结论、### 考试线索、### 易错点、### 复习清单。\n\
             3. **宁缺毋滥**：每条内容都要能落到转写原文；教科书上本来就有的通用定义（如函数三要素、极限定义）只在老师当场强调时才记，并注明是老师强调；口语冗余、重复内容直接舍弃。\
             章节间上下文连贯的可与之前章节互相引用（如「延续 11-03 的积分法」）。\n\
             4. 例题与讲解（理科课）：逐题记【题目（尽力还原式子）】【解法步骤】【易错点/老师提醒】；转写破碎只对缺失处标「待核对」，能还原的部分照常写，不要整段放弃。\
             文科课（思修/五史/职规等无例题的课）改用「知识点与问答」：把要点整理成可自测的问答对（Q：… A：…），便于考前盖住答案自测。\n\
             5. 题型与方法：把本课出现的题型归纳成「题型 → 方法/步骤」短条目（如「判断两个函数是否相同 → 先看定义域，再看对应法则」），这是期末复习最先看的部分。\n\
             6. 考试线索：优先收老师的考试信号原话——「会考」「出大题」「分值」「题型分布」，以及「这部分不考/不要求证明」之类的排除信息（排除信息同样省复习时间）；每条注明是老师原话还是你依据转写的推断。\n\
             7. 易错点：老师点名的常见错误、课上有人做错的地方（如「把式子错误拆开导致解不出 $x$」），一句话说清错误做法与正确做法。\n\
             8. 复习清单：每条必须对应本课出现过的具体题目或考点，可执行可勾选（如「重做求反函数那题：由 $y=\\ln(Bx+C)+A$ 解出 $x$」）；禁止「默写/背出某定义」这类泛泛条目，除非老师当场要求背。\n\
             9. 数学内容：口语化数学表达转规范记号（「根号二」→ $\\sqrt{{2}}$、「x的平方」→ $x^2$、分式/积分/极限用 LaTeX，Obsidian 可直接渲染）；ASR 同音误听（极限/基线、求导、ln 等）按上下文修正。\n\
             10. **不臆想**：只依据转写原文整理，课堂没讲的内容不补充、不外推；无法辨认或疑似转写错误的片段如实标注「待核对」，不要编造顺滑的假内容；若整段转写与学科无关（静音幻觉、字幕署名等），只写一行说明该段无可整理内容。\n\
             11. 完成后简单汇报本次笔记要点与最终写入路径（不超过 10 行），不要复述全文。\n\n\
             以下是本次转写原文：\n\n{transcript}",
            vault_dir = self.config.vault_dir.trim_end_matches('/'),
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
        // bridge：插件 cdylib 带独立 tokio 副本，网络 I/O 必须跑在插件自己的
        // runtime 里（宿主 runtime 的 reactor TLS 跨不过 .so 边界）。
        let owned = self.clone();
        ru41::plugins::bridge(async move { owned.poll_once().await })
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
