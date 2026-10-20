use crate::domain::NotifyRoute;
use anyhow::{Context, Result};
use serde::Deserialize;

use super::service::BlogConfig;

/// 博客发布管线配置；blog poll/run 使用。
/// 博客仓库只存服务器本地克隆路径与远端名，凭据走 git credential helper。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BlogSection {
    /// 服务器上的博客仓库克隆路径（容器内路径，如 /blog）。
    pub repo_path: Option<String>,
    /// 远端与分支；默认 origin/master。
    pub remote: Option<String>,
    pub branch: Option<String>,
    /// Vault 内的已发布目录（相对路径），也是发布触发入口；
    /// 把定稿 markdown 移入该目录并推送即发布。默认 06-Blog-博客/Published-已发布。
    pub published_dir: Option<String>,
    /// 站点 URL（发布成功通知里拼链接）；默认 https://windy664.github.io。
    pub site_url: Option<String>,
    /// blog run 的轮询间隔（秒），默认 300。
    pub interval_secs: Option<u64>,
    /// 发布完成后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
}

impl BlogSection {
    pub const DEFAULT_INTERVAL_SECS: u64 = 300;
    pub const DEFAULT_REMOTE: &str = "origin";
    pub const DEFAULT_BRANCH: &str = "master";
    pub const DEFAULT_PUBLISHED_DIR: &str = "06-Blog-博客/Published-已发布";
    pub const DEFAULT_SITE_URL: &str = "https://windy664.github.io";
    /// Astro content collection 的文章目录。
    pub const POSTS_DIR: &str = "source/_posts";

    pub fn is_ready(&self) -> bool {
        self.repo_path
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
    }

    /// 由 [sources.blog] 解析出运行时配置；默认值只定义在这里。
    /// 依赖核心 [obsidian] 的 vault_path（已发布文件在 Vault 仓库里）。
    pub fn resolved(&self, notify: Option<NotifyRoute>) -> Result<BlogConfig> {
        let repo_path = self
            .repo_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .context("sources.blog.repo_path required")?;
        Ok(BlogConfig {
            repo_path: repo_path.into(),
            remote: self
                .remote
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_REMOTE.into()),
            branch: self
                .branch
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_BRANCH.into()),
            published_dir: self
                .published_dir
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_PUBLISHED_DIR.into()),
            posts_dir: Self::POSTS_DIR.into(),
            site_url: self
                .site_url
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_SITE_URL.into()),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify,
        })
    }
}
