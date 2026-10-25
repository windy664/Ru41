//! YouTube 英语预习附属：频道新视频 → yt-dlp 字幕转写 → Agent 重点词句预习单 → 推送链接。
//!
//! 与 rss 的差异：YouTube RSS 端点对本部署不可用（404），采集与字幕统一走 yt-dlp 子进程；
//! 每个 video 独立入队一个预习任务（而非合并 digest），终态由任务通知出口推送。

pub mod config;
pub mod qq_command;
pub mod review;
pub mod service;
pub mod source;

pub use config::YoutubeSection;
pub use review::YoutubeReviewJob;
pub use service::{YoutubeConfig, YoutubePollReport, YoutubeTracker};

/// yt-dlp 客户端工厂：tracker、QQ 命令与 CLI prep 共用同一份配置来源。
pub fn ytdlp_client(config: &crate::bootstrap::config::Config) -> source::YtDlpClient {
    let youtube = &config.sources.youtube;
    source::YtDlpClient::new(
        youtube.ytdlp_bin.clone().unwrap_or_else(|| "yt-dlp".into()),
        youtube.proxy_url.clone(),
    )
}

/// 注册表定义：youtube 的调度清单、tracker 构造、QQ 命令都在本模块内闭环。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "youtube",
    schedule: |config| {
        let youtube = &config.sources.youtube;
        let ready = youtube.channels.as_ref().is_some_and(|c| !c.is_empty());
        vec![
            super::JobSchedule::simple(
                "youtube",
                youtube
                    .interval_secs
                    .unwrap_or(config::YoutubeSection::DEFAULT_INTERVAL_SECS),
                ready,
                (!ready).then(|| "配置 sources.youtube.channels 后启用".into()),
            ),
            // 复习闭环：词块落库 + 间隔复习推送 + 看后测试（内部按日历日幂等）。
            super::JobSchedule::simple(
                "youtube-review",
                6 * 3600,
                ready,
                (!ready).then(|| "配置 sources.youtube.channels 后启用".into()),
            ),
        ]
    },
    tracker: |config, store, name| {
        let youtube = &config.sources.youtube;
        match name {
            "youtube" => Ok(Some(Box::new(YoutubeTracker::new(
                store,
                youtube.resolved(crate::bootstrap::assembly::source_notify(
                    config,
                    youtube.notify,
                )?)?,
            )))),
            "youtube-review" => Ok(Some(Box::new(review::review_job(
                store,
                config,
                &youtube.resolved(crate::bootstrap::assembly::source_notify(
                    config,
                    youtube.notify,
                )?)?,
            )?))),
            _ => Ok(None),
        }
    },
    qq_handlers: |config, backend| {
        Ok(vec![std::sync::Arc::new(qq_command::YoutubeQqCommand::new(
            ytdlp_client(config),
            config.sources.youtube.interests.clone().unwrap_or_default(),
            config
                .sources
                .youtube
                .level
                .clone()
                .unwrap_or_else(|| config::YoutubeSection::DEFAULT_LEVEL.into()),
            config
                .sources
                .youtube
                .max_transcript_chars
                .unwrap_or(config::YoutubeSection::DEFAULT_MAX_TRANSCRIPT_CHARS),
            backend.to_owned(),
        ))])
    },
};
