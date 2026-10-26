use crate::{
    app::{digest::keyword_match, task::TaskService},
    domain::NotifyRoute,
    ports::storage::TaskStore,
};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use super::source::{
    TranscriptSegment, VideoEntry, VideoMeta, YtDlpClient, format_ts, render_transcript,
};

/// YouTube 英语预习配置（对应配置文件的 [sources.youtube]，由 YoutubeSection 解析）。
#[derive(Debug, Clone)]
pub struct YoutubeConfig {
    /// 归一化后的频道 videos 列表页。
    pub channels: Vec<String>,
    /// 标题关键词粗筛；空列表不过滤。
    pub keywords: Vec<String>,
    /// 个人关注方向描述，注入预习单 prompt。
    pub interests: String,
    /// 学习者英语水平描述。
    pub level: String,
    /// 短于该秒数的视频跳过（过滤 shorts）。
    pub min_duration_secs: u64,
    pub max_per_channel: u32,
    pub max_per_poll: u32,
    /// 视频发布时间的最大回溯天数；0 表示不做时效窗口。
    pub max_age_days: u64,
    pub max_transcript_chars: usize,
    pub interval_secs: u64,
    pub backend: String,
    pub notify: Option<NotifyRoute>,
    /// 每日日报推送时刻（本地 HH:MM）。
    pub digest_at: String,
    pub ytdlp_bin: String,
    pub proxy_url: Option<String>,
}

/// 一轮轮询的结果报告；单频道/单视频失败不中断，错误收进 errors。
#[derive(Debug, Serialize)]
pub struct YoutubePollReport {
    pub channels: usize,
    pub fetched: usize,
    /// 本轮新登记的视频数（含被过滤/失败的，均已登记去重）。
    pub new_videos: usize,
    /// 本轮成功入队的预习任务数。
    pub prepared: usize,
    pub errors: Vec<String>,
    pub task_ids: Vec<i64>,
}

/// seen 记录状态机：pending（字幕待拉/重试）→ done（已入队）/ failed（重试耗尽）；
/// filtered（关键词未命中）/ short（时长不足）/ old（超新鲜度窗口）为终态跳过；
/// baseline（首次接触频道的存量快照）永不入队——自动追踪只追新，存量用 /预习 手动点播。
const SEEN_KIND: &str = "youtube_seen";
/// 频道首见基线：首次轮询只登记不预习，杜绝历史积压刷屏。
const BASELINE_KIND: &str = "youtube_baseline";
const MAX_TRANSCRIPT_ATTEMPTS: u64 = 3;

/// YouTube 英语预习追踪：频道列表 → 去重与粗筛 → 字幕转写 → 逐视频入队预习任务。
/// 终态推送即 Agent 输出的预习单原文（含视频链接），经 [notify] 默认出口投递。
pub struct YoutubeTracker {
    store: Arc<dyn TaskStore>,
    client: YtDlpClient,
    config: YoutubeConfig,
}

impl YoutubeTracker {
    pub fn new(store: Arc<dyn TaskStore>, config: YoutubeConfig) -> Self {
        let client = YtDlpClient::new(config.ytdlp_bin.clone(), config.proxy_url.clone());
        Self {
            store,
            client,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<YoutubePollReport> {
        let mut fetched = 0;
        let mut new_videos = 0;
        let mut errors = Vec::new();
        let mut candidates = Vec::new();
        for channel in &self.config.channels {
            let videos = match self
                .client
                .list_videos(channel, self.config.max_per_channel)
                .await
            {
                Ok(videos) => videos,
                Err(error) => {
                    errors.push(format!("{channel}: {error:#}"));
                    continue;
                }
            };
            fetched += videos.len();
            // 首次接触频道：只登记存量快照（baseline），不预习任何历史视频。
            let is_new_channel = match self.store.document(BASELINE_KIND, channel) {
                Ok(None) => true,
                Ok(_) => false,
                Err(error) => {
                    errors.push(format!("{channel}: {error:#}"));
                    continue;
                }
            };
            if is_new_channel {
                for video in &videos {
                    // 覆盖式登记：旧版本存量 seen（如历史积压 pending）一并改写为 baseline。
                    if let Err(error) =
                        self.upsert_seen(channel, &video.id, "baseline", &video.title)
                    {
                        errors.push(format!("{}#{}: {error:#}", channel, video.id));
                    }
                }
                if let Err(error) = self.store.put_document(
                    BASELINE_KIND,
                    channel,
                    None,
                    &serde_json::json!({
                        "baseline_at": crate::util::now(),
                        "items": videos.len(),
                    }),
                ) {
                    errors.push(format!("{channel}: {error:#}"));
                }
                new_videos += videos.len();
                continue;
            }
            for video in videos {
                match self.register_seen(channel, &video) {
                    Ok(SeenDecision::Candidate) => {
                        new_videos += 1;
                        candidates.push((channel.clone(), video));
                    }
                    Ok(SeenDecision::Registered) => new_videos += 1,
                    Ok(SeenDecision::Skip) => {}
                    Err(error) => errors.push(format!("{}#{}: {error:#}", channel, video.id)),
                }
            }
        }
        candidates.truncate(self.config.max_per_poll as usize);

        let cutoff = if self.config.max_age_days == 0 {
            None
        } else {
            Some(crate::util::now() - (self.config.max_age_days as i64) * 86_400)
        };
        let mut prepared = 0;
        let mut task_ids = Vec::new();
        for (channel, video) in candidates {
            // flat 列表通常缺发布时间：预习前补一次元数据，超窗的老视频只登记不入队。
            let timestamp = match video.timestamp {
                Some(ts) => Some(ts),
                None => match self.client.video_meta(&video.id).await {
                    Ok(meta) => meta.timestamp,
                    Err(error) => {
                        errors.push(format!("meta {}: {error:#}", video.id));
                        None
                    }
                },
            };
            if let (Some(ts), Some(cutoff)) = (timestamp, cutoff)
                && (ts as i64) < cutoff
            {
                self.mark_seen(&channel, &video.id, "old", 0);
                continue;
            }
            match self.client.transcript(&video.id).await {
                Ok(segments) if !segments.is_empty() => {
                    match self.enqueue_preview(&channel, &video, &segments) {
                        Ok(task_id) => {
                            prepared += 1;
                            task_ids.push(task_id);
                            self.mark_seen(&channel, &video.id, "done", 0);
                        }
                        Err(error) => errors.push(format!("enqueue {}#: {error:#}", video.id)),
                    }
                }
                Ok(_) => self.record_transcript_failure(&channel, &video, "空转写", &mut errors),
                Err(error) => self.record_transcript_failure(
                    &channel,
                    &video,
                    &format!("{error:#}"),
                    &mut errors,
                ),
            }
        }
        Ok(YoutubePollReport {
            channels: self.config.channels.len(),
            fetched,
            new_videos,
            prepared,
            errors,
            task_ids,
        })
    }

    /// 去重登记与候选判定：新视频按关键词/时长终态登记或列为候选；
    /// pending 的历史记录在重试额度内再次候选（字幕拉取是网络操作，失败可重试）。
    fn register_seen(&self, channel: &str, video: &VideoEntry) -> Result<SeenDecision> {
        let seen_key = format!("{channel}#{}", video.id);
        if let Some(doc) = self.store.document(SEEN_KIND, &seen_key)? {
            return match doc.value["status"].as_str() {
                Some("pending")
                    if doc.value["attempts"].as_u64().unwrap_or(0) < MAX_TRANSCRIPT_ATTEMPTS =>
                {
                    Ok(SeenDecision::Candidate)
                }
                Some("pending") => {
                    self.mark_seen(
                        channel,
                        &video.id,
                        "failed",
                        doc.value["attempts"].as_u64().unwrap_or(0),
                    );
                    Ok(SeenDecision::Skip)
                }
                _ => Ok(SeenDecision::Skip),
            };
        }
        let status = if !keyword_match(&self.config.keywords, &video.title) {
            "filtered"
        } else if video
            .duration_secs
            .is_some_and(|d| d < self.config.min_duration_secs)
        {
            "short"
        } else {
            self.write_seen(channel, &video.id, "pending", 0, &video.title)?;
            return Ok(SeenDecision::Candidate);
        };
        self.write_seen(channel, &video.id, status, 0, &video.title)?;
        Ok(SeenDecision::Registered)
    }

    fn record_transcript_failure(
        &self,
        channel: &str,
        video: &VideoEntry,
        reason: &str,
        errors: &mut Vec<String>,
    ) {
        let seen_key = format!("{channel}#{}", video.id);
        let attempts = self
            .store
            .document(SEEN_KIND, &seen_key)
            .ok()
            .flatten()
            .and_then(|doc| doc.value["attempts"].as_u64())
            .unwrap_or(0)
            + 1;
        let status = if attempts >= MAX_TRANSCRIPT_ATTEMPTS {
            "failed"
        } else {
            "pending"
        };
        self.mark_seen(channel, &video.id, status, attempts);
        errors.push(format!("transcript {}: {reason}", video.id));
    }

    fn write_seen(
        &self,
        channel: &str,
        video_id: &str,
        status: &str,
        attempts: u64,
        title: &str,
    ) -> Result<bool> {
        let seen_key = format!("{channel}#{video_id}");
        self.store.put_document(
            SEEN_KIND,
            &seen_key,
            None,
            &serde_json::json!({
                "status": status,
                "attempts": attempts,
                "title": title,
                "first_seen": crate::util::now(),
            }),
        )
    }

    /// 幂等更新（CAS 失败仅见于并发轮询，忽略即可）。
    fn mark_seen(&self, channel: &str, video_id: &str, status: &str, attempts: u64) {
        let seen_key = format!("{channel}#{video_id}");
        if let Ok(Some(doc)) = self.store.document(SEEN_KIND, &seen_key) {
            let mut value = doc.value.clone();
            value["status"] = status.into();
            value["attempts"] = attempts.into();
            let _ = self
                .store
                .put_document(SEEN_KIND, &seen_key, Some(doc.revision), &value);
        }
    }

    /// 存在则改写状态（保留 attempts 等字段），不存在则新建。
    fn upsert_seen(&self, channel: &str, video_id: &str, status: &str, title: &str) -> Result<()> {
        let seen_key = format!("{channel}#{video_id}");
        match self.store.document(SEEN_KIND, &seen_key)? {
            Some(doc) => {
                let mut value = doc.value.clone();
                value["status"] = status.into();
                let _ =
                    self.store
                        .put_document(SEEN_KIND, &seen_key, Some(doc.revision), &value)?;
            }
            None => {
                self.write_seen(channel, video_id, status, 0, title)?;
            }
        }
        Ok(())
    }

    /// 拉好的转写合成预习单 prompt 并入队；任务幂等键按 video_id。
    fn enqueue_preview(
        &self,
        channel: &str,
        video: &VideoEntry,
        segments: &[TranscriptSegment],
    ) -> Result<i64> {
        let transcript = render_transcript(segments, self.config.max_transcript_chars);
        // 学习者模型：已毕业词块不再进预习单（known_chunks 由复习反馈积累）；
        // 级别用 /定级 的评测结果，配置里的静态 level 只做兜底。
        let known = super::review::known_chunks(&self.store, 80);
        let level = super::review::current_level(&self.store, &self.config.level);
        let prompt = preview_prompt(
            &self.config.interests,
            &level,
            channel,
            video,
            &transcript,
            &known,
        );
        let key = format!("youtube:prep:{}", video.id);
        let mut service = TaskService::new(self.store.clone());
        // 自动轮询的预习单不带即时通知——统一进每日 12:10 日报（review job 汇总）。
        // 手动 /预习 仍走即时推送（prep_video_task 带 route）。
        let result = service.enqueue_for(&key, &prompt, &self.config.backend)?;
        Ok(result.task.id)
    }
}

enum SeenDecision {
    /// 新登记且通过筛选，列为本轮候选。
    Candidate,
    /// 新登记但被终态过滤（关键词/短时长），只登记不入队。
    Registered,
    /// 已登记过，跳过。
    Skip,
}

/// 手动点播一个视频：拉元数据与转写并入队预习任务（QQ `/预习` 与 CLI `youtube prep` 共用）。
/// 幂等键与轮询路径一致（`youtube:prep:<id>`），同一视频重复提交返回既有任务。
#[allow(clippy::too_many_arguments)]
pub async fn prep_video_task(
    store: &Arc<dyn TaskStore>,
    client: &YtDlpClient,
    video_id: &str,
    interests: &str,
    level: &str,
    max_transcript_chars: usize,
    backend: &str,
    route: Option<&NotifyRoute>,
) -> Result<String> {
    let meta = client
        .video_meta(video_id)
        .await
        .unwrap_or_else(|_| VideoMeta {
            title: video_id.to_owned(),
            duration_secs: None,
            timestamp: None,
        });
    let segments = match client.transcript(video_id).await {
        Ok(segments) if !segments.is_empty() => segments,
        Ok(_) => return Ok("未能获取该视频的英文字幕（转写为空），可能不是英文视频。".into()),
        Err(error) => return Ok(format!("拉取字幕失败：{error:#}")),
    };
    let video = VideoEntry {
        id: video_id.to_owned(),
        title: meta.title,
        duration_secs: meta.duration_secs,
        view_count: None,
        timestamp: meta.timestamp,
    };
    let transcript = render_transcript(&segments, max_transcript_chars);
    // /预习 手动点播同样吃定级结果，保持与频道轮询同一学习者模型。
    let level = &super::review::current_level(store, level);
    let prompt = preview_prompt(interests, level, "手动提交", &video, &transcript, &[]);
    let key = format!("youtube:prep:{video_id}");
    let mut service = TaskService::new(store.clone());
    let result = match route {
        Some(route) => service.enqueue_with_notify(&key, &prompt, backend, route)?,
        None => service.enqueue_for(&key, &prompt, backend)?,
    };
    Ok(if result.created {
        format!(
            "预习任务已创建：#{}《{}》，完成后推送预习单",
            result.task.id, video.title
        )
    } else {
        format!("该视频已有预习任务：#{}《{}》", result.task.id, video.title)
    })
}

/// 预习单 prompt：推送文本即 Agent 输出原文，因此链接/格式都要求写进输出。
/// 时间戳硬约束「逐字照抄行标记」缓解模型自行换算的漂移；代码锚定留给 v2。
fn preview_prompt(
    interests: &str,
    level: &str,
    channel: &str,
    video: &VideoEntry,
    transcript: &str,
    known_chunks: &[String],
) -> String {
    let url = format!("https://www.youtube.com/watch?v={}", video.id);
    let duration = video
        .duration_secs
        .map(format_ts)
        .unwrap_or_else(|| "未知".into());
    let interests = if interests.is_empty() {
        String::new()
    } else {
        format!("个人关注方向：{interests}\n")
    };
    let known = if known_chunks.is_empty() {
        String::new()
    } else {
        format!(
            "学习者已掌握以下词块，不要收录：{}\n",
            known_chunks.join("、")
        )
    };
    format!(
        "你在为中国大学生（英语水平约 {level}）生成 YouTube 视频的「精听预习单」。\n\
         他先读预习单再去看视频；目的不是背单词，而是看懂这期视频、扫清听力障碍。频道：{channel}\n\
         {interests}\
         {known}\
         视频标题：{title}\n\
         视频链接：{url}\n\
         视频时长：{duration}\n\n\
         输出格式（这就是最终推送的消息，不要输出格式以外的任何内容）：\n\
         📺 {{一句话主线：这期视频到底在说什么}}\n\
         🔗 {url}\n\
         ⏱ {{时长}}｜⏳ 学习时间：预习 5 分钟 + 观看 + 看后检测 5 分钟\n\n\
         🗺 听什么（按视频顺序 2-4 段，每段一行）：\n\
         - [m:ss] {{这段在做什么}} → 抓住：{{听时要抓的信号：数字/立场词/转折/术语，只指路不下结论，别提前说出看后检测的答案}}\n\n\
         📖 现场装备（4-6 个，宁少勿滥，只收理解这期视频的关键；按长期迁移价值排序，前 2-3 个为 ⭐ 必学——离开视频在工作/技术/日常依然高频；⭐ 条目将进入间隔复习，其余为场景词只帮理解）：\n\
         1. **词块（用原形，如 stick around，别照抄原句时态）** /{{美式 IPA，元音带长度符号（/uː/ /iː/ /ɑː/），与词典记法一致；仅 ⭐ 必学条目标注}}/ {{在这期视频里具体指什么，结合情境说人话；速度/程度类修饰若来自上下文（如 in a few minutes），注明出处，别固化进释义}}；\"完整原句\" [m:ss]\n\
         （「别误解为」仅当该词块存在真实混淆点时才写，写在释义后；没有就整条省略，禁止凑数）\n\n\
         🔗 重点单词（3-4 个，宁缺毋滥：只收「认识但不会主动用」的词——重点给词形变化与高频搭配；学习者天天见的术语禁收）：\n\
         1. **单词** /IPA/ {{词性}} {{中文释义}}；搭配 {{高频搭配}}；词形 {{常见变形}}；\"完整原句\" [m:ss]\n\
         \n\
         🔁 带走就用（2-3 个可迁移口语块）：\n\
         - **\"句式或短语（原形）\"** {{什么场合能用；句式类给一行结构公式（如「Verb + sb + into + doing」）并点出语法陷阱（如 get around to 的 to 是介词，后接 V-ing）}} → 你可以说：\"{{贴合大学生生活的英文例句}}\"；\"完整原句\" [m:ss]\n\n\
         \n\
         🧩 重点句式（凑满 2 个，素材实在不足才给 1 个；从原文挑可整体迁移的句法骨架，禁硬凑）：\n\
         - **\"get + sb/sth + to + do\"** {{通用语法功能一句话（能指物也能指人），再一句点出视频里的具体义；语法要点一句话}} → 你可以说：\"{{贴合大学生生活的英文例句}}\"；\"完整原句\" [m:ss]\n\
         \n\
         🎤 口语任务：用今天的带走块或句式之一，说一句你自己的真实情况（别写稿，10 秒内说完）。\n\
         \n\
         👂 精听挑战（2-3 处；每个锚点三步：①不看字幕盲听写词 ②对照字幕找漏听 ③0.75-1 倍速跟读模仿）：\n\
         - [m:ss] \"{{完整原句}}\" → {{预期音变：哪个音连到哪个音/哪个词弱化成什么/吞掉什么音；区分整词弱读与词内元音弱读（如 the 弱读是元音变 /ə/、辅音 /ð/ 仍在）}}（现象：{{连读|弱读|闪音|失爆|同化|缩读|重音|语调|其他}}）\n\n\
         ❓ 听前自测：{{一个带着去看视频的具体问题}}\n\n\
         ✅ 看后检测（看完视频再回来做，3-4 题）：\n\
         1. {{内容理解三选一 A/B/C，考主线或关键细节，选项用中文}}\n\
         2. {{英文填空，用今天某个 ⭐ 必学表达，给中文提示}}\n\
         3. {{用今天的重点单词或重点句式造一句（或改写一道）}}\n\
         4. {{中译英一句，指定用某个今天的表达}}\n\
         答案：{{一行写完，如 1B 2 gearing up 3 I powered through it.}}\n\
         硬性要求：\n\
         - 转写每行行首形如 [L12 2:14]；引用的时间戳必须逐字照抄自对应行标记，禁止自己换算。\n\
         - 引用的原句必须逐字出现在转写里，必须是完整句子或语义完整的片段（可跨字幕行拼接），禁止在短语中间截断出半句话。\n\
         - 仅 ⭐ 必学条目（离开视频依然高频、必须会造句）标注美式 IPA，其余场景词看懂即可、禁止标音标。
         - 字幕不是音频：锚点只能写「预期音变」，禁止断言主播某秒的真实发音；跨句衔接可能有停顿，锚点只选同一句内的衔接。重音/语调类点评信息焦点（哪个词承载笑点或重点），不凭空描写音高曲线。\n\
         - 听力锚点行尾的现象标签必须从给定的 8 个里选一个，禁止自创。\n\
         - 现场装备按「不懂它就看不懂这期视频」挑选；带走就用按「他能马上用上」挑选，两者不要重复。\n\
         - 词块只收「语言表达」：短语动词、习语、搭配、口语句式、听力难点。概念名词与产品/技术名（AI factories、CUDA-X、physical AI 这类）不算词块——听得出、查得到，不构成语言障碍，禁止收录，需要交代就写进「听什么」的抓住里。\n\
         - PR、root cause、deploy、tracker、offload、sandbox、harness、inference 这类工程/AI 通用词默认视为已知（学习者是计算机/AI 背景），不占条目；仅当视频中的用法有特殊含义时收录，且必须点明特殊在哪。\n\
         - 学习价值优先于固定数量：某类真实素材不足就少收，禁止为凑数收录低价值条目或硬造句式分析。
         - 带走块、精听锚点、口语任务尽量围绕同一句原文展开（词汇-句式-听力-口语一个闭环），不要各选各的句子。
         - 技术术语与项目名的拼写必须照抄字幕原文，禁止按印象改写。
         - 全文 1700 字以内。\n\n\
         == 英文字幕转写 ==\n{transcript}",
        level = level,
        channel = channel,
        interests = interests,
        known = known,
        title = video.title,
        url = url,
        duration = duration,
        transcript = transcript,
    )
}

impl crate::ingress::schedule::PollJob for YoutubeTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}
