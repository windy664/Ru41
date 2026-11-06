//! YouTube 英语学习附属：频道新视频 → yt-dlp 字幕转写 → Agent 重点词句学习单 → 推送链接。
//! QQ 主入口 `/英语 <链接>`（`/预习` 保留为兼容别名）。
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
pub fn ytdlp_client(config: &ru41::bootstrap::config::Config) -> source::YtDlpClient {
    let youtube = config
        .sources
        .get_or_default::<crate::YoutubeSection>("youtube");
    source::YtDlpClient::new(
        youtube.ytdlp_bin.clone().unwrap_or_else(|| "yt-dlp".into()),
        youtube.proxy_url.clone(),
        youtube.ytdlp_cookies.clone(),
    )
}

/// 注册表定义：youtube 的调度清单、tracker 构造、QQ 命令都在本模块内闭环。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "youtube",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("youtube"),
    schedule: |config| {
        let youtube = config
            .sources
            .get_or_default::<crate::YoutubeSection>("youtube");
        let ready = youtube.channels.as_ref().is_some_and(|c| !c.is_empty());
        vec![
            ru41::modules::JobSchedule::simple(
                "youtube",
                youtube
                    .interval_secs
                    .unwrap_or(config::YoutubeSection::DEFAULT_INTERVAL_SECS),
                ready,
                (!ready).then(|| "配置 sources.youtube.channels 后启用".into()),
            ),
            // 复习闭环：词块落库 + 15 分钟一跳的每日日报（预习单/复习/专题打包一条）。
            ru41::modules::JobSchedule::simple(
                "youtube-review",
                15 * 60,
                ready,
                (!ready).then(|| "配置 sources.youtube.channels 后启用".into()),
            ),
        ]
    },
    tracker: |config, store, name| {
        let youtube = config.sources.get::<crate::YoutubeSection>("youtube")?;
        match name {
            "youtube" => Ok(Some(Box::new(YoutubeTracker::new(
                store,
                youtube.resolved(ru41::bootstrap::assembly::source_notify(
                    config,
                    youtube.notify,
                )?)?,
            )))),
            "youtube-review" => Ok(Some(Box::new(review::review_job(
                store,
                config,
                &youtube.resolved(ru41::bootstrap::assembly::source_notify(
                    config,
                    youtube.notify,
                )?)?,
            )?))),
            _ => Ok(None),
        }
    },
    qq_handlers: |config, backend| {
        Ok(vec![std::sync::Arc::new(
            qq_command::YoutubeQqCommand::new(
                ytdlp_client(config),
                config
                    .sources
                    .get::<crate::YoutubeSection>("youtube")?
                    .interests
                    .clone()
                    .unwrap_or_default(),
                config
                    .sources
                    .get::<crate::YoutubeSection>("youtube")?
                    .level
                    .clone()
                    .unwrap_or_else(|| config::YoutubeSection::DEFAULT_LEVEL.into()),
                config
                    .sources
                    .get::<crate::YoutubeSection>("youtube")?
                    .max_transcript_chars
                    .unwrap_or(config::YoutubeSection::DEFAULT_MAX_TRANSCRIPT_CHARS),
                backend.to_owned(),
            ),
        )])
    },
    image_consumers: ru41::modules::no_image_consumers,
    cli: youtube_cli,
};

/// 专属 CLI：`ru41 youtube prep <链接|视频ID>`——手动点播一个视频，
/// 与 QQ `/英语` 同源（幂等键一致，重复提交返回既有任务）。
fn youtube_cli(
    config: &ru41::bootstrap::config::Config,
    store: std::sync::Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = anyhow::Result<Option<serde_json::Value>>> + Send>,
> {
    use anyhow::Context as _;
    let prepared = (|| -> anyhow::Result<_> {
        anyhow::ensure!(
            args.first().map(String::as_str) == Some("prep"),
            "用法: ru41 youtube prep <链接|视频ID>"
        );
        let url = args
            .get(1)
            .context("用法: ru41 youtube prep <链接|视频ID>")?;
        let video_id = qq_command::parse_video_id(url).with_context(|| {
            format!("无法从「{url}」解析视频 ID（支持 watch / youtu.be / shorts 链接或 11 位 ID）")
        })?;
        let youtube = config.sources.get::<crate::YoutubeSection>("youtube")?;
        let route = ru41::bootstrap::assembly::source_notify(config, youtube.notify)?;
        Ok((
            video_id,
            ytdlp_client(config),
            youtube.interests.clone().unwrap_or_default(),
            youtube
                .level
                .clone()
                .unwrap_or_else(|| config::YoutubeSection::DEFAULT_LEVEL.into()),
            youtube
                .max_transcript_chars
                .unwrap_or(config::YoutubeSection::DEFAULT_MAX_TRANSCRIPT_CHARS),
            youtube.backend.clone().unwrap_or_else(|| "pi".into()),
            route,
        ))
    })();
    Box::pin(ru41::plugins::bridge(async move {
        let (video_id, client, interests, level, max_chars, backend, route) = prepared?;
        let message = service::prep_video_task(
            &store,
            &client,
            &video_id,
            &interests,
            &level,
            max_chars,
            &backend,
            route.as_ref(),
        )
        .await?;
        Ok(Some(
            serde_json::json!({ "video_id": video_id, "message": message }),
        ))
    }))
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("youtube", || DEF)
}
