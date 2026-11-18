//! 转写 worker：单轮 poll 消费一条 pending 录音。
//! ffmpeg 切片（单声道 16k mp3）→ ASR 逐片转写 → 转写内联进 agent prompt
//! → agent 把结构化笔记写进 Vault。vault 里只沉淀笔记；转写与音频都是临时产物。

use crate::asr::AsrClient;
use crate::config::LectureConfig;
use crate::pending::{
    PENDING_KIND, PENDING_SCAN_LIMIT, PendingItem, STATUS_DOWNLOADING, oldest_unfinished,
    sanitize_name,
};
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

/// 下载超过这个秒数还没翻成 pending，就判失败（多半是容器重启把下载掐了）。
const DOWNLOAD_STALE_SECS: i64 = 1800;

/// 通知用户某条录音没下下来，请重发。
///
/// 插件不直连渠道 API（红线），走 worker 同款「入队 + 通知」通道，
/// 让 agent 把失败原因转述给用户——改成后台下载后，失败不再有即时回执兜着，
/// 静默失败会让人以为还在排队。
pub fn notify_download_failed(
    store: &dyn TaskStore,
    backend: &str,
    item: &PendingItem,
    reason: &str,
) {
    let prompt = format!(
        "课堂录音《{}》下载失败：{reason}\n\
         QQ 的文件链接有时效，多半要用户重新发一次。\n\
         请直接回复用户一句话：这份录音没能下下来（原因），请重新发一次；不要展开分析。",
        item.filename
    );
    if let Err(err) = store.enqueue_with_notify(
        &format!("lecture:download-failed:{}", item.msg_id),
        &prompt,
        backend,
        ru41::domain::RunContract::AnalysisV1,
        &ru41::domain::NotifyRoute {
            channel: item.channel.clone(),
            target: item.target.clone(),
            msg_id: Some(item.msg_id.clone()),
        },
    ) {
        eprintln!("[lecture] 下载失败通知入队失败：{err:#}");
    }
}

/// 按上课时间匹配课表上的课程。
///
/// handler（即时回执）与 worker（最终归档）共用这一份实现：
/// 回执里说的课程名和最后写进 Vault 的课程必然一致。
pub fn match_course(
    store: &Arc<dyn TaskStore>,
    options: TodoOptions,
    config: &LectureConfig,
    recorded_at: i64,
) -> String {
    let lead = 10 * 60;
    let service = ScheduleService::new(store.clone(), options);
    let Ok(occurrences) = service.occurrences_between(recorded_at - lead, recorded_at + lead, 20)
    else {
        return config.fallback_course.clone();
    };
    let containing = occurrences
        .iter()
        .find(|o| o.starts_at - lead <= recorded_at && recorded_at < o.ends_at);
    containing
        .map(|o| sanitize_name(&o.title))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| config.fallback_course.clone())
}

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
        let now = ru41::util::now();
        // 每轮收尾：卡在 downloading 的判失败 + 回收过期原始音频。
        let (stale, reaped) = self.sweep_queue(now)?;
        // 队列按**收到时间**排队：先到的先转。
        // 不能用 documents() 的返回顺序（按 id 排，id 是 QQ 的随机 msg_id）。
        let Some((doc, item)) = oldest_unfinished(self.store.as_ref())? else {
            return Ok(
                serde_json::json!({"processed": null, "stale_failed": stale, "audio_reaped": reaped}),
            );
        };
        let course = self.course_at(item.recorded_time());
        match self.transcribe(&item).await {
            Ok(transcript) => {
                // 转写只存在于内存与 agent prompt，不落 vault。
                let enqueued = self.enqueue_agent(&item, &course, &transcript)?;
                // 状态、课程、转写全文**一次 CAS 更新**落库。
                // （原来分两次 update：第一次把 revision 推上去了，第二次还拿着旧
                // revision → CAS 必失败 → 转写从未落库、工作区也从不清理。
                // 2026-11-16 线上实测：29 条记录 transcript 全是 NULL、切片堆了 1.1G、
                // 每轮 poll 都报「状态更新竞争失败」。）
                self.mark_done(&doc, &item, &course, &transcript.text)?;
                // 清理只删磁盘大头：切片立刻删（每片几十 MB），原始音频按保留期留，
                // 转写文本留着。整目录删掉会让重跑变成不可能。
                let work = self.config.workspace.join(&item.msg_id);
                self.cleanup_workspace(&work, &item, now);
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

    /// 转写完成后落库：状态、课程、转写全文一次 CAS 更新写完。
    ///
    /// 转写必须持久化进记录：工作区会被清理，转写不能只活在磁盘上——
    /// 丢了就再也补不回来（2026-11-09 丢过一节网络空间安全导论的转写，
    /// 连原始录音一起没了，笔记只能写「待补」）。
    fn mark_done(
        &self,
        doc: &ru41::domain::records::Document,
        item: &PendingItem,
        course: &str,
        transcript: &str,
    ) -> Result<()> {
        self.update(&doc.revision, item, |v| {
            v.status = "done".into();
            v.course = Some(course.to_owned());
            v.error = None;
            v.transcript = Some(transcript.to_owned());
        })
    }

    /// 每轮扫一遍待办队列，做两件收尾：
    ///
    /// 1. 把「下载中」超时未完成的判失败并通知用户重发——
    ///    下载在后台跑（2026-11-16 起），容器重启/进程被杀会把它掐在半路，
    ///    那条待办会永远停在 downloading：worker 跳过它，用户还以为在下载。
    /// 2. 回收过期的原始音频——`cleanup_workspace` 只在转写当下跑一次，
    ///    那时 age_days=0，「过期再删」的条件永远不满足，原始音频就永远留着
    ///    （一节课几十 MB，一学期几个 G）。这里按 received_at 删。
    ///
    /// 返回（判失败的条数, 回收的原始音频条数）。
    fn sweep_queue(&self, now: i64) -> Result<(usize, usize)> {
        let mut failed = 0;
        let mut reaped = 0;
        let retain_days = self.config.retain_audio_days as i64;
        for doc in self.store.documents(PENDING_KIND, PENDING_SCAN_LIMIT)? {
            let Ok(item) = PendingItem::from_document(&doc) else {
                continue;
            };
            // ① 卡在 downloading 超时的
            if item.status == STATUS_DOWNLOADING && now - item.received_at >= DOWNLOAD_STALE_SECS {
                let mut next = item.clone();
                next.status = "failed".into();
                next.error = Some("下载超时未完成（服务可能重启过）".into());
                let written = self.store.put_document(
                    PENDING_KIND,
                    &item.msg_id,
                    Some(doc.revision),
                    &serde_json::to_value(&next)?,
                )?;
                if written {
                    failed += 1;
                    eprintln!("[lecture] 下载超时未完成，判失败：{}", item.msg_id);
                    notify_download_failed(
                        self.store.as_ref(),
                        &self.config.backend,
                        &next,
                        "下载超时未完成（服务可能重启过）",
                    );
                }
                continue;
            }
            // ② 原始音频过保留期就删（retain_days=0 表示一律不留）
            let age_days = (now - item.received_at).max(0) / 86_400;
            if !(retain_days == 0 || age_days >= retain_days) {
                continue;
            }
            let raw = self.config.workspace.join(&item.msg_id).join("raw");
            if raw.is_dir() && std::fs::remove_dir_all(&raw).is_ok() {
                reaped += 1;
                eprintln!("[lecture] 原始音频过保留期，删除：{}", item.msg_id);
            }
        }
        Ok((failed, reaped))
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

    /// 工作区清理：删切片（磁盘大头），原始音频过保留期再删；转写文本保留。
    pub fn cleanup_workspace(&self, work: &std::path::Path, item: &PendingItem, now: i64) {
        // 切片：每片一段 mp3，几十 MB 起步，立刻删。
        let slices = work.join("slices");
        if let Err(error) = std::fs::remove_dir_all(&slices)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("[lecture] 清理切片失败（不影响流程）：{error:#}");
        }
        // 原始音频：按保留期留，过期再删（转写有疑问还能重跑 ASR）。
        let retain_days = self.config.retain_audio_days;
        let age_days = (now - item.received_at).max(0) / 86_400;
        if retain_days == 0 || age_days >= retain_days as i64 {
            let raw = work.join("raw");
            if let Err(error) = std::fs::remove_dir_all(&raw)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                eprintln!("[lecture] 清理原始录音失败（不影响流程）：{error:#}");
            }
        }
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
        match_course(&self.store, self.options, &self.config, recorded_at)
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
        let prompt = build_note_prompt(&NotePrompt {
            course,
            course_line: &course_line,
            recorded_label: &recorded_label,
            recorded_source: item.recorded_time_source(),
            date: &date,
            stem: &stem,
            note_path: &note_path,
            transcript_source: &transcript_source,
        });
        self.store.enqueue_with_notify(
            &format!("lecture:notes:{}", item.msg_id),
            &prompt,
            &self.config.backend,
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

/// 组装「整理课堂笔记」的 prompt。
/// 抽成独立函数：提示词是要反复调的东西，能直接单测比只能读代码强。
#[derive(Clone, Copy)]
struct NotePrompt<'a> {
    course: &'a str,
    course_line: &'a str,
    recorded_label: &'a str,
    recorded_source: &'a str,
    date: &'a str,
    stem: &'a str,
    note_path: &'a str,
    transcript_source: &'a str,
}

fn build_note_prompt(p: &NotePrompt<'_>) -> String {
    let NotePrompt {
        course,
        course_line,
        recorded_label,
        recorded_source,
        date,
        stem,
        note_path,
        transcript_source,
    } = *p;
    format!(
        "这是《{course}》的课堂录音转写，请帮我整理成这门课的课堂笔记。\n\
             用途只有一个：期末复习拿分——宁可短而实，不要长而空。\n\
             - 这门课：{course}\n\
             - 录音时间：{recorded_label}（{recorded_source}；课程匹配与归档日期的依据）\n\
             {course_line}\n\
             - 日期：{date}\n\
             - 目标文件（Vault 绝对路径）：{note_path}\n\
             要求：\n\
             0. **必须写上面给的绝对路径**：写进 Vault 仓库，不要写当前工作目录（当前目录是任务沙箱，写在这里不会进 Vault，笔记会丢）。课程归属以课表核对为准：直接写入上面的目标文件，**不要按转写内容改课程、不要新建其他课程目录**。\
             若转写内容与核对课程明显对不上，笔记照常写入目标文件，并在汇报末尾加一行「⚠️ 课程核对异常：<内容更像哪门课>」交人工确认。\n\
             1. 目标文件不存在就创建（带 # {course} 课堂笔记 标题）；已存在就读取后**在末尾追加**新章节，其他章节已有内容一字不动。\n\
             1b. **同名章节已经在了就替换，别再挂一个**：如果文件里已经有 `## {date} {stem}` 这一节\
             （同一节课被重新整理过，或同一段录音重发过），把那一节的内容**整体替换**成这次整理的结果\
             （标题保留原样），不要在末尾再追加一个同名章节——重复章节比旧版本更糟。\n\
             2. 新章节格式：`## {date} {stem}`。**别套模板**：标题由内容定，讲了什么用什么标题；\
             没有合适标题的地方就用连贯段落或短条目，别硬套「本课概览／题型与方法／易错点／复习清单」这一套固定栏目，\
             也不要为了凑齐栏目去写。下面只是可能用得上的内容方向，不是必须出现的部分——这节课真讲了才写，\
             没讲的整段不写（不留空标题、不写「无」）：这节课实际讲了什么、例题与讲解、题型与方法、\
             公式与结论、考试线索、易错点、复习清单。\
             写的是本人复习用的笔记，不是交上去的材料：不要「综上所述」「希望对你有帮助」这类套话，\
             末尾也不用加「本笔记由 AI 生成」之类的声明。\n\
             3. **宁缺毋滥**：每条内容都要能落到转写原文；教科书上本来就有的通用定义（如函数三要素、极限定义）只在老师当场强调时才记，并注明是老师强调；口语冗余、重复内容直接舍弃。\
         3b. **同一件事只写一遍**：正文已经写清的，题型/易错点/复习清单里只在**确有增量**时再提\
         （如答题套路、错误做法与正确做法、可直接背的原话），换个说法重述一遍不算增量。\
         问答对只收正文没展开的；复习清单只收需要动手做的，别把正文再列一遍。\
         3c. **与本课主题和考试无关的课堂内容不写**：哲学闲聊、课堂纪律与举报、房价/八卦闲谈、\
         老师的个人经历、同学的发言——除非老师当场把它点成考点或案例。\
         3d. **课堂投票、现场互动是教学过程，不是考场论据**，不要写成答题要用的材料。\
             章节间上下文连贯的可与之前章节互相引用（如「延续 11-03 的积分法」）。\n\
             4. 例题与讲解（理科课）：逐题记【题目（尽力还原式子）】【解法步骤】【易错点/老师提醒】；转写破碎只对缺失处标「待核对」，能还原的部分照常写，不要整段放弃。\
             文科课（思修/五史/职规等无例题的课）改用「知识点与问答」：把要点整理成可自测的问答对（Q：… A：…），便于考前盖住答案自测。\n\
             5. 题型与方法：把本课出现的题型归纳成「题型 → 方法/步骤」短条目（如「判断两个函数是否相同 → 先看定义域，再看对应法则」），这是期末复习最先看的部分。\n\
             6. 考试线索：优先收老师的考试信号原话——「会考」「出大题」「分值」「题型分布」，以及「这部分不考/不要求证明」之类的排除信息（排除信息同样省复习时间）；每条注明是老师原话还是你依据转写的推断。\n\
             7. 易错点：老师点名的常见错误、课上有人做错的地方（如「把式子错误拆开导致解不出 $x$」），一句话说清错误做法与正确做法。\n\
             8. 复习清单：每条必须对应本课出现过的具体题目或考点，可执行可勾选（如「重做求反函数那题：由 $y=\\ln(Bx+C)+A$ 解出 $x$」）；禁止「默写/背出某定义」这类泛泛条目，除非老师当场要求背。\n\
         **只收能确认的**：转写里还「待核对」的内容**不要进清单**（等于让人去背一个可能错的东西），\n\
         要核对的集中放到末尾「待核对」一小节，正文里点出它对应正文哪一句。\n\
             9. 数学内容：口语化数学表达转规范记号（「根号二」→ $\\sqrt{{2}}$、「x的平方」→ $x^2$）；\
             行内公式用单个 `$…$`，独立成行的公式用 `$$` 独占一行的块公式——\
             **不要在句子里写 `$$…$$`**（Obsidian 渲染会坏成一坨）；\
             分式、积分、极限、求和一律 LaTeX，Obsidian 可直接渲染；\
             ASR 同音误听（极限/基线、求导、ln 等）按上下文修正。\n\
             10. **不臆想**：只依据转写原文整理，课堂没讲的内容不补充、不外推；无法辨认或疑似转写错误的片段如实标注「待核对」，不要编造顺滑的假内容；若整段转写与学科无关（静音幻觉、字幕署名等），只写一行说明该段无可整理内容。\n\
             11. 写完把改动提交进 Vault 仓库（提交信息前缀 `[ru41]`，例如 `[ru41] 课堂笔记：{date} {course}`）。\n\
             12. 完成后简单汇报本次笔记要点与最终写入路径（不超过 10 行），不要复述全文。\n\n\
             {transcript_source}",
    )
}

impl ru41::ingress::schedule::SimplePollJob for LectureWorker {
    type Report = serde_json::Value;

    fn interval(&self) -> std::time::Duration {
        Duration::from_secs(crate::config::LectureSection::DEFAULT_INTERVAL_SECS)
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        self.poll_once().await
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

/// 归档命名：日期取录音时间（文件名解析出的开录时刻，回退收到时间）的本地日期；stem 来自原始文件名。
#[cfg(test)]
mod tests {
    use super::{NotePrompt, TRANSCRIPT_INLINE_LIMIT, Transcript, build_note_prompt};
    use std::path::PathBuf;

    /// 笔记提示词必须管住三件容易失控的事：**不重复**、**不写闲谈**、**待核对的不进清单**。
    ///
    /// 2026-11-18 国家安全教育那节实测：同一批内容在正文/问答/题型/清单里重复 4~5 遍
    /// （占 46% 篇幅），还混进了哲学闲聊、举报老师、房价闲谈，
    /// 且「华人局」这类待核对的案名被排进了必背清单。
    #[test]
    fn note_prompt_forbids_repetition_chatter_and_unverified_checklist_items() {
        let prompt = build_note_prompt(&NotePrompt {
            course: "国家安全教育",
            course_line: "- 课表核对课程：国家安全教育",
            recorded_label: "2026-11-18 17:03",
            recorded_source: "文件名",
            date: "2026-11-18",
            stem: "语音 261118_170347",
            note_path: "/vault/01-Courses-课程/x/课堂笔记.md",
            transcript_source: "以下是本次转写原文：\n\n老师讲…",
        });
        assert!(prompt.contains("同一件事只写一遍"), "{prompt}");
        assert!(prompt.contains("与本课主题和考试无关的课堂内容不写"), "{prompt}");
        assert!(prompt.contains("不要进清单"), "{prompt}");
        assert!(prompt.contains("不是考场论据"), "{prompt}");
    }

    /// 同名章节已存在时要**替换**，不能再追加一个同名章节。
    ///
    /// 2026-11-18 实测：重跑同一节课时，agent 看到 `## 2026-11-18 语音 261118_170347`
    /// 已经在文件里，就判定「内容已覆盖」整节不动——旧版本里那些重复和闲谈原样留着，
    /// 重跑等于白跑。同一段录音重发时也一样会撞出两个同名章节。
    #[test]
    fn note_prompt_replaces_existing_same_name_section() {
        let prompt = build_note_prompt(&NotePrompt {
            course: "国家安全教育",
            course_line: "- 课表核对课程：国家安全教育",
            recorded_label: "2026-11-18 17:03",
            recorded_source: "文件名",
            date: "2026-11-18",
            stem: "语音 261118_170347",
            note_path: "/vault/01-Courses-课程/x/课堂笔记.md",
            transcript_source: "以下是本次转写原文：\n\n老师讲…",
        });
        assert!(prompt.contains("同名章节已经在了就替换"), "{prompt}");
        assert!(prompt.contains("整体替换"), "{prompt}");
        assert!(prompt.contains("不要在末尾再追加一个同名章节"), "{prompt}");
    }

    /// 每轮收尾要回收过期原始音频。
    ///
    /// `cleanup_workspace` 只在转写当下跑一次，那时 age_days=0，
    /// 「过期再删」的条件永远不满足——原始音频就永远留着（每节课几十 MB）。
    #[test]
    fn sweep_reaps_expired_audio() {
        use ru41::adapters::database::sqlite::SqliteTaskStore;
        use ru41::ports::storage::TaskStore;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
        let mut config = crate::config::LectureSection {
            api_key: Some("test-key".into()),
            ..Default::default()
        }
        .resolved()
        .unwrap();
        config.workspace = tmp.path().to_path_buf();
        let worker = super::LectureWorker::new(
            store.clone(),
            ru41::domain::todo::TodoOptions::default(),
            config,
        );
        let now = ru41::util::now();
        let item = crate::pending::PendingItem {
            file: String::new(),
            filename: "a.m4a".into(),
            size: 1,
            channel: "qq".into(),
            target: "u".into(),
            msg_id: "old".into(),
            // 8 天前：过了 7 天保留期。
            received_at: now - 8 * 86_400,
            status: "done".into(),
            error: None,
            course: None,
            transcript: None,
        };
        store
            .put_document(
                crate::pending::PENDING_KIND,
                "old",
                None,
                &serde_json::to_value(&item).unwrap(),
            )
            .unwrap();
        let raw = tmp.path().join("old").join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        std::fs::write(raw.join("a.m4a"), b"x").unwrap();
        let (_stale, reaped) = worker.sweep_queue(now).unwrap();
        assert_eq!(reaped, 1, "过期原始音频应被回收");
        assert!(!raw.exists(), "过期原始音频应删掉");
    }

    /// 状态、课程、转写全文必须**一次 CAS 更新**落库。
    /// 原来分两次 update：第一次把 revision 推上去了，第二次还拿着旧 revision
    /// → CAS 必失败 → 转写从未落库、工作区也从不清理（2026-11-16 线上实测：
    /// 29 条记录 transcript 全是 NULL、切片堆了 1.1G、每轮 poll 报竞争失败）。
    #[test]
    fn done_state_lands_with_transcript_in_one_update() {
        use ru41::adapters::database::sqlite::SqliteTaskStore;
        use ru41::ports::storage::TaskStore;
        use std::sync::Arc;

        let tmp = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
        let mut config = crate::config::LectureSection {
            api_key: Some("test-key".into()),
            ..Default::default()
        }
        .resolved()
        .unwrap();
        config.workspace = tmp.path().to_path_buf();
        let worker = super::LectureWorker::new(
            store.clone(),
            ru41::domain::todo::TodoOptions::default(),
            config,
        );
        let item = crate::pending::PendingItem {
            file: String::new(),
            filename: "a.mp3".into(),
            size: 1,
            channel: "qq".into(),
            target: "u".into(),
            msg_id: "m1".into(),
            received_at: ru41::util::now(),
            status: "pending".into(),
            error: None,
            course: None,
            transcript: None,
        };
        store
            .put_document(
                crate::pending::PENDING_KIND,
                "m1",
                None,
                &serde_json::to_value(&item).unwrap(),
            )
            .unwrap();
        let doc = store
            .document(crate::pending::PENDING_KIND, "m1")
            .unwrap()
            .unwrap();
        worker
            .mark_done(&doc, &item, "高等数学", "老师讲了导数")
            .unwrap();
        let after = store
            .document(crate::pending::PENDING_KIND, "m1")
            .unwrap()
            .unwrap();
        let saved = crate::pending::PendingItem::from_document(&after).unwrap();
        assert_eq!(saved.status, "done");
        assert_eq!(saved.course.as_deref(), Some("高等数学"));
        assert_eq!(
            saved.transcript.as_deref(),
            Some("老师讲了导数"),
            "转写全文必须一起落库（2026-11-16 之前它从没落过）"
        );
    }

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

    /// 提示词必须带课程语境、并且明确禁止套模板；行内公式不许用 `$$`。
    #[test]
    fn prompt_carries_course_context_and_forbids_templates() {
        let prompt = build_note_prompt(&NotePrompt {
            course: "高等数学I(理)",
            course_line: "- 课表核对课程：高等数学I(理)",
            recorded_label: "2026-11-05 10:41",
            recorded_source: "文件名时间",
            date: "2026-11-05",
            stem: "上午10点41分",
            note_path: "/vault/01-Courses-课程/高等数学I(理)/课堂笔记.md",
            transcript_source: "以下是本次转写原文：\n\n老师讲了导数。",
        });
        assert!(prompt.contains("《高等数学I(理)》"));
        assert!(prompt.contains("这门课：高等数学I(理)"));
        assert!(prompt.contains("别套模板"));
        assert!(prompt.contains("不要在句子里写 `$$…$$`"));
        // 旧的固定栏目写法不能再出现，否则等于把模板又塞回去。
        assert!(!prompt.contains("栏目按需取用"));
    }
}
