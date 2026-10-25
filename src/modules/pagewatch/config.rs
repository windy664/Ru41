use anyhow::{Context, Result};
use serde::Deserialize;

use super::service::{PagewatchConfig, WatchedPage};

/// 页面监控配置（[sources.pagewatch]）：盯没有 RSS 的网页（赛事官网、公告页），
/// 内容指纹变化即通知。pagewatch poll / pagewatch run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PagewatchSection {
    /// 监控的页面列表：name 用于通知文案，url 抓取地址。
    pub pages: Option<Vec<PageEntry>>,
    /// 轮询间隔（秒），默认 1800。
    pub interval_secs: Option<u64>,
    /// 内容变化是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PageEntry {
    pub name: String,
    pub url: String,
}

impl PagewatchSection {
    pub const DEFAULT_INTERVAL_SECS: u64 = 1800;

    pub fn is_ready(&self) -> bool {
        self.pages.as_ref().is_some_and(|pages| !pages.is_empty())
    }

    /// 由 [sources.pagewatch] 解析出运行时配置；通知路由由装配层注入。
    pub fn resolved(&self, notify: Option<crate::domain::NotifyRoute>) -> Result<PagewatchConfig> {
        let pages = self
            .pages
            .clone()
            .filter(|pages| !pages.is_empty())
            .context("sources.pagewatch.pages required")?;
        Ok(PagewatchConfig {
            pages: pages
                .into_iter()
                .map(|entry| WatchedPage {
                    name: entry.name,
                    url: entry.url,
                })
                .collect(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify,
        })
    }
}
