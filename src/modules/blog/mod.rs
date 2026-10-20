//! 博客发布管线：Obsidian `Pending-待发布/` 文件夹就是发布入口。
//! 用户在 Obsidian 写完移入该文件夹并 git 同步，服务器轮询发现后经确定性校验
//! 写入博客仓库 `source/_posts/` 并推送（Astro 站点自动构建），
//! 随后把文件移入 `Published-已发布/` 并以 `[ru41]` 提交回 Vault。
//! 两个仓库之间只有单向通道（Vault → 博客），博客 → Vault 的已发布同步由
//! `Published-已发布/` 一次性拷入即可，不做自动回环。

pub mod config;
pub mod service;

pub use config::BlogSection;
pub use service::{BlogConfig, BlogPollReport, BlogPublishTracker};
