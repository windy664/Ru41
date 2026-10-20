//! 博客发布管线：Obsidian `Published-已发布/` 文件夹就是发布入口。
//! 用户在 Obsidian 写完定稿、从 `Drafts-草稿/` 移入该目录并 git 同步，
//! 服务器轮询发现后经确定性校验写入博客仓库 `source/_posts/` 并推送
//! （Astro 站点自动构建）。没有中间队列目录，文件始终留在已发布目录归档。
//! 两个仓库之间只有单向通道（Vault → 博客），不做自动回环。

pub mod config;
pub mod service;

pub use config::BlogSection;
pub use service::{BlogConfig, BlogPollReport, BlogPublishTracker};
