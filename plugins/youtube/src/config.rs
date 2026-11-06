use anyhow::{Context, Result};
use ru41::domain::NotifyRoute;
use serde::Deserialize;

use crate::service::YoutubeConfig;

/// YouTube 英语预习配置；youtube poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeSection {
    /// 频道列表：`@handle` 或频道 URL（自动补 /videos 路径）。
    pub channels: Option<Vec<String>>,
    /// 标题关键词粗筛（大小写不敏感）；空列表表示不过滤。
    pub keywords: Option<Vec<String>>,
    /// 个人关注方向描述，注入预习单 prompt。
    pub interests: Option<String>,
    /// 学习者英语水平描述（注入 prompt），默认 "CET-6 / B2"。
    pub level: Option<String>,
    /// 短于此秒数的视频跳过（过滤 shorts），默认 60。
    pub min_duration_secs: Option<u64>,
    /// 每个频道单轮最多检查的最新视频数，默认 15。
    pub max_per_channel: Option<u32>,
    /// 首见某个频道时，把「时效窗口内」的视频也当作候选去预习。
    /// 默认关：首见只登记存量快照，避免一次性灌进一堆历史视频。
    /// 换频道后想立刻有内容可学时打开它。
    pub backfill_on_first_sight: Option<bool>,
    /// 屏蔽的频道（handle 或频道名，忽略大小写）：不巡查、不预习、不进日报。
    /// 用户点名不要的频道放这里（2026-11-06：BBC Learning English 的新闻节目）。
    pub blocked_channels: Option<Vec<String>>,
    /// 每个频道要巡查的标签页。默认 videos + streams：
    /// 很多 UP 主主更新在直播页（/videos 只放常规上传），只查 videos 会漏掉他们。
    pub tabs: Option<Vec<String>>,
    pub max_per_poll: Option<u32>,
    /// 视频发布时间的最大回溯天数（超窗只登记不预习）；0 表示不做时效窗口，默认 14。
    pub max_age_days: Option<u64>,
    /// 注入 prompt 的转写最大字符数，默认 24000。
    pub max_transcript_chars: Option<usize>,
    /// youtube run 的轮询间隔（秒），默认 21600（6 小时）。
    pub interval_secs: Option<u64>,
    /// 预习任务的执行底座，默认 pi。
    pub backend: Option<String>,
    /// 终态后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// 每日日报推送时刻（本地时间 HH:MM）：新预习单 + 到期复习 + 发音专题打包一条，默认 "12:10"。
    pub digest_at: Option<String>,
    /// 晚间补充推送时刻（本地时间 HH:MM），默认 "20:30"。
    ///
    /// 只有两个固定时刻会推：中午主推送 + 晚间补充。窗口外一律不推。
    pub digest_evening_at: Option<String>,
    /// yt-dlp 可执行文件路径，默认 PATH 上的 "yt-dlp"。
    pub ytdlp_bin: Option<String>,
    /// 传给 yt-dlp 子进程的代理（HTTPS_PROXY/HTTP_PROXY）；缺省继承 ru41 进程环境。
    pub proxy_url: Option<String>,
    /// yt-dlp --cookies 文件路径（Netscape 格式）；缺省不带 cookies。
    pub ytdlp_cookies: Option<String>,
}

impl YoutubeSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 3_600;

    /// 学习者水平默认描述（QQ 命令与 CLI 共用）。
    pub const DEFAULT_LEVEL: &str = "CET-6 / B2";
    /// 每日日报默认推送时刻。
    pub const DEFAULT_DIGEST_AT: &str = "12:10";
    /// 晚间补充推送默认时刻。
    pub const DEFAULT_DIGEST_EVENING_AT: &str = "20:30";
    /// 注入 prompt 的转写默认上限。
    pub const DEFAULT_MAX_TRANSCRIPT_CHARS: usize = 24_000;
    /// 新鲜度窗口默认天数；自动追踪只追新，存量内容走手动 /预习。
    pub const DEFAULT_MAX_AGE_DAYS: u64 = 14;
    /// 默认巡查的标签页：常规上传 + 直播（很多 UP 主主更新在直播页）。
    pub const DEFAULT_TABS: [&'static str; 2] = ["videos", "streams"];
    /// 由 [sources.youtube] 解析出运行时配置；默认值只定义在这里。
    /// 通知路由由装配层按 [notify] 渠道注入，模块内不感知渠道知识。
    pub fn resolved(&self, notify: Option<NotifyRoute>) -> Result<YoutubeConfig> {
        Ok(YoutubeConfig {
            channels: self
                .channels
                .clone()
                .filter(|c| !c.is_empty())
                .context("sources.youtube.channels required")?
                .iter()
                .map(|entry| channel_videos_url(entry))
                .collect(),
            backfill_on_first_sight: self.backfill_on_first_sight.unwrap_or(false),
            blocked_channels: self
                .blocked_channels
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.trim().to_owned())
                .filter(|c| !c.is_empty())
                .collect(),
            tabs: self
                .tabs
                .clone()
                .filter(|tabs| !tabs.is_empty())
                .unwrap_or_else(|| Self::DEFAULT_TABS.iter().map(|s| s.to_string()).collect()),
            keywords: self.keywords.clone().unwrap_or_default(),
            interests: self.interests.clone().unwrap_or_default(),
            level: self
                .level
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_LEVEL.into()),
            min_duration_secs: self.min_duration_secs.unwrap_or(60),
            max_per_channel: self.max_per_channel.unwrap_or(15),
            max_per_poll: self.max_per_poll.unwrap_or(6),
            max_age_days: self.max_age_days.unwrap_or(Self::DEFAULT_MAX_AGE_DAYS),
            max_transcript_chars: self
                .max_transcript_chars
                .unwrap_or(Self::DEFAULT_MAX_TRANSCRIPT_CHARS),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            backend: self.backend.clone().unwrap_or_else(|| "pi".into()),
            notify,
            digest_at: self
                .digest_at
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_DIGEST_AT.into()),
            digest_evening_at: self
                .digest_evening_at
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_DIGEST_EVENING_AT.into()),
            ytdlp_bin: self.ytdlp_bin.clone().unwrap_or_else(|| "yt-dlp".into()),
            proxy_url: self.proxy_url.clone(),
            ytdlp_cookies: self.ytdlp_cookies.clone(),
        })
    }
}

/// 频道入口归一化为 videos 列表页：`@handle` → URL；已有 URL 补 /videos。
fn channel_videos_url(entry: &str) -> String {
    let entry = entry.trim().trim_end_matches('/');
    if entry.starts_with("http") {
        if entry.ends_with("/videos") {
            entry.to_string()
        } else {
            format!("{entry}/videos")
        }
    } else {
        format!("https://www.youtube.com/{entry}/videos")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_channel_entries() {
        assert_eq!(
            channel_videos_url("@Fireship"),
            "https://www.youtube.com/@Fireship/videos"
        );
        assert_eq!(
            channel_videos_url("https://www.youtube.com/@Fireship"),
            "https://www.youtube.com/@Fireship/videos"
        );
        assert_eq!(
            channel_videos_url("https://www.youtube.com/@Fireship/videos"),
            "https://www.youtube.com/@Fireship/videos"
        );
    }

    #[test]
    fn resolves_defaults_and_requires_channels() {
        let section = YoutubeSection {
            channels: Some(vec!["@Fireship".into()]),
            ytdlp_cookies: Some("/data/yt.txt".into()),
            ..Default::default()
        };
        let config = section.resolved(None).unwrap();
        assert_eq!(config.interval_secs, YoutubeSection::DEFAULT_INTERVAL_SECS);
        assert_eq!(config.max_per_poll, 6);
        assert_eq!(config.min_duration_secs, 60);
        assert_eq!(config.max_age_days, YoutubeSection::DEFAULT_MAX_AGE_DAYS);
        assert_eq!(config.level, "CET-6 / B2");
        assert_eq!(config.ytdlp_bin, "yt-dlp");
        assert_eq!(config.ytdlp_cookies.as_deref(), Some("/data/yt.txt"));

        assert!(YoutubeSection::default().resolved(None).is_err());
    }
}
