//! YouTube 英语预习附属：频道新视频 → yt-dlp 字幕转写 → Agent 重点词句预习单 → 推送链接。
//!
//! 与 rss 的差异：YouTube RSS 端点对本部署不可用（404），采集与字幕统一走 yt-dlp 子进程；
//! 每个 video 独立入队一个预习任务（而非合并 digest），终态由任务通知出口推送。

pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::YoutubeSection;
pub use service::{YoutubeConfig, YoutubePollReport, YoutubeTracker};
