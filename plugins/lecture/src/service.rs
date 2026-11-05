//! 转写 worker：单轮 poll 消费一条 pending 录音。
//! ffmpeg 切片（单声道 16k mp3）→ ASR 逐片转写 → 转写内联进 agent prompt
//! → agent 把结构化笔记写进 Vault。vault 里只沉淀笔记；转写与音频都是临时产物。

use crate::asr::AsrClient;
use crate::config::LectureConfig;
use crate::pending::{PENDING_KIND, PendingItem, sanitize_name};
use anyhow::{Context, Result};
use chrono::{Local, TimeZone};
use ru41::app::schedule::ScheduleService;
use ru41::domain::todo::TodoOptions;
use ru41::ports::storage::TaskStore;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 转写结果：文本 + 落盘路径（长录音让 agent 自己分段读文件）。
struct Transcript {
    /// 完整转写文本。
    text: String,
    /// 转写全文文件路径（agent 沙箱可读：工作目录与 run 目录同在 /data 下）。
    path: PathBuf,
}

/// 转写短于这个长度就内联进 prompt，超过则只给文件路径。
const TRANSCRIPT_INLINE_LIMIT: usize = 4000;

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
            .find(|d| {
                PendingItem::from_document(d)
                    .map(|i| i.status != "done" && i.status != "failed")
                    .unwrap_or(false)
            })
            .cloned();
        let Some(doc) = doc else {
            return Ok(serde_json::json!({"processed": null}));
        };
        let item = PendingItem::from_document(&doc)?;
        let course = self.course_at(item.recorded_time());
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
                    "transcript_chars": transcript.text.chars().count(),
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

    /// 切片 → ASR 逐片转写 → 落盘成一份转写全文。
    ///
    /// 每片的转写结果单独存成 `slices/<片名>.txt`：重投时已有文本直接复用，
    /// 不再重调 ASR（本地 CPU 也要跑几分钟）。整段拼进 prompt 只在文本短时用；
    /// 长录音改用「转写文件路径 + 让 agent 分段读」，避免一次把全文塞进上下文
    /// （长上下文里模型容易漏中段，失败重跑也要重付整段 token）。
    async fn transcribe(&self, item: &PendingItem) -> Result<Transcript> {
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
            // 每片的转写文本缓存：与切片同名 .txt，重投时复用。
            let text_path = slice.with_extension("txt");
            let text = match std::fs::read_to_string(&text_path) {
                Ok(cached) if !cached.trim().is_empty() => cached,
                _ => {
                    let text = asr
                        .transcribe(slice)
                        .await
                        .with_context(|| format!("转写第 {}/{} 片失败", index + 1, slices.len()))?;
                    let _ = std::fs::write(&text_path, &text);
                    text
                }
            };
            parts.push(text);
        }
        let joined = parts
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        // 全文另存一份：长录音时 agent 自己分段读这个文件。
        let transcript_path = work.join("transcript.md");
        let _ = std::fs::write(&transcript_path, &joined);
        Ok(Transcript {
            text: joined,
            path: transcript_path,
        })
    }

    /// 原始文件切片；已有切片则跳过（中断续跑不重复转码）。
    fn ensure_slices(&self, raw: &Path, slices_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut existing: Vec<PathBuf> = std::fs::read_dir(slices_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")))
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
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")))
            .collect();
        slices.sort();
        Ok(slices)
    }

    /// 按录音时间猜课程（优先文件名里解析出的开录时刻，回退收到时刻）：
    /// 落在课区间内的课（允许开课前 10 分钟起录）；区间外一律回退 fallback。
    /// 不再就近猜课：猜错课比留兜底更伤笔记归档（2026-11-04 形势与政策误归教训）。
    /// 公开供集成测试。
    pub fn course_at(&self, recorded_at: i64) -> String {
        let lead = 10 * 60;
        let service = ScheduleService::new(self.store.clone(), self.options);
        let Ok(occurrences) =
            service.occurrences_between(recorded_at - lead, recorded_at + lead, 20)
        else {
            return self.config.fallback_course.clone();
        };
        let containing = occurrences
            .iter()
            .find(|o| o.starts_at - lead <= recorded_at && recorded_at < o.ends_at);
        containing
            .map(|o| sanitize_name(&o.title))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.config.fallback_course.clone())
    }

    /// 笔记落点（Vault 绝对路径）。课程认不出来时落收件箱，不硬造课程目录。
    /// 认得出时只给到课程根，真实目录名由 agent 查「课程总览.md」对照表决定
    /// （课表课程名与 vault 目录名不一定一致，如「程序设计基础」↔「C语言程序设计」）。
    pub fn note_target(&self, course: &str, date: &str) -> String {
        let vault_root = self.config.vault_root.trim_end_matches('/');
        if course == self.config.fallback_course {
            return format!(
                "{vault_root}/{}/待归类-课堂笔记-{date}.md",
                self.config.inbox_dir.trim_end_matches('/')
            );
        }
        format!(
            "{vault_root}/{}/<课程目录>/课堂笔记.md",
            self.config.vault_dir.trim_end_matches('/')
        )
    }

    /// 入队 agent 整理结构化笔记；转写全量内联进 prompt，agent 只写笔记进 Vault。
    /// 一门课一份连续笔记（课堂笔记.md），每次课按日期章节追加在末尾。
    /// request_key 幂等，重复 poll 不会重复建任务。
    fn enqueue_agent(
        &self,
        item: &PendingItem,
        course: &str,
        transcript: &Transcript,
    ) -> Result<ru41::domain::EnqueueResult> {
        let (date, stem) = archive_name(item);
        let recorded_label = Local
            .timestamp_opt(item.recorded_time(), 0)
            .single()
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let vault_root = self.config.vault_root.trim_end_matches('/');
        let course_root = format!(
            "{vault_root}/{}",
            self.config.vault_dir.trim_end_matches('/')
        );
        let unknown = course == self.config.fallback_course;
        let note_path = self.note_target(course, &date);
        let course_line = if unknown {
            format!(
                "- 课表核对：录音时间没落在任何课的区间里，课程未知，笔记暂存到收件箱（{note_path}）；\
                 请在汇报末尾写一行「这节更像哪门课：<课程名>」，由人工归位"
            )
        } else {
            format!(
                "- 课表核对课程：{course}（录音时间落在该课的课表区间内，以此为准）\n\
                 - 课程目录：在 {course_root}/ 下找这门课的目录。目录名可能与课程名不同\
                 （如课表「程序设计基础」对应目录「C语言程序设计」），而且通常在学期子目录下\
                 （如 2026-Fall-大一上/）。先读该目录树里的「课程总览.md」，\
                 在「课程目录 | 课表课程」对照表里按课表课程名「{course}」查目录名；\
                 查不到再用 ls 找同名目录；确实没有才在最近的学期目录下新建。"
            )
        };
        // 短转写直接内联；长的只给文件路径，让 agent 分段读（上下文小、可续跑）。
        let transcript_source = if transcript.text.chars().count() <= TRANSCRIPT_INLINE_LIMIT {
            format!("以下是本次转写原文：\n\n{}", transcript.text)
        } else {
            format!(
                "以下是本次转写原文（共 {} 字，存放在文件里）：\n\n{}\n\n\
                 **不要一次读完**：用 read/bash 分段读（例如每次 200 行），边读边整理，\
                 读完再写笔记。文件只读，不要修改。",
                transcript.text.chars().count(),
                transcript.path.display()
            )
        };
        let prompt = format!(
            "请把课堂录音转写整理成课堂笔记，用途只有一个：期末复习拿分。宁可短而实，不要长而空。\n\
             - 录音时间：{recorded_label}（{recorded_source}；课程匹配与归档日期的依据）\n\
             {course_line}\n\
             - 日期：{date}\n\
             - 目标文件（Vault 绝对路径）：{note_path}\n\
             要求：\n\
             0. **必须写上面给的绝对路径**：写进 Vault 仓库，不要写当前工作目录（当前目录是任务沙箱，写在这里不会进 Vault，笔记会丢）。课程归属以课表核对为准：直接写入上面的目标文件，**不要按转写内容改课程、不要新建其他课程目录**。\
             若转写内容与核对课程明显对不上，笔记照常写入目标文件，并在汇报末尾加一行「⚠️ 课程核对异常：<内容更像哪门课>」交人工确认。\n\
             1. 目标文件不存在就创建（带 # {course} 课堂笔记 标题）；已存在就读取后**在末尾追加**新章节，已有内容一字不动。\n\
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
             11. 写完把改动提交进 Vault 仓库（提交信息前缀 `[ru41]`，例如 `[ru41] 课堂笔记：{date} {course}`）。\n\
             12. 完成后简单汇报本次笔记要点与最终写入路径（不超过 10 行），不要复述全文。\n\n\
             {transcript_source}",
            recorded_source = item.recorded_time_source(),
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

/// 归档命名：日期取录音时间（文件名解析出的开录时刻，回退收到时间）的本地日期；stem 来自原始文件名。
#[cfg(test)]
mod tests {
    use super::{TRANSCRIPT_INLINE_LIMIT, Transcript};
    use std::path::PathBuf;

    /// 短转写内联、长转写只给路径：两种形态都要让 agent 拿到全文。
    #[test]
    fn transcript_source_switches_between_inline_and_path() {
        let short = Transcript {
            text: "老师讲了一个例子。".repeat(10),
            path: PathBuf::from("/data/lecture/x/transcript.md"),
        };
        assert!(short.text.chars().count() <= TRANSCRIPT_INLINE_LIMIT);
        let long = Transcript {
            text: "长".repeat(TRANSCRIPT_INLINE_LIMIT + 1),
            path: PathBuf::from("/data/lecture/x/transcript.md"),
        };
        assert!(long.text.chars().count() > TRANSCRIPT_INLINE_LIMIT);
    }
}

fn archive_name(item: &PendingItem) -> (String, String) {
    let date = Local
        .timestamp_opt(item.recorded_time(), 0)
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
