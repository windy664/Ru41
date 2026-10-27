use ru41::domain::NotifyRoute;
use anyhow::{Context, Result};
use serde::Deserialize;

use crate::service::PapersConfig;

/// arXiv 论文追踪配置；papers poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArxivSection {
    /// 追踪的分类，如 ["cs.CR", "cs.AI", "cs.CL"]。
    pub categories: Option<Vec<String>>,
    /// 关键词粗筛；空或缺省表示不过滤。
    pub keywords: Option<Vec<String>>,
    /// 个人关注方向描述，注入 digest prompt 供 Agent 判断相关性。
    pub interests: Option<String>,
    /// 单次拉取条数上限，默认 100。
    pub max_results: Option<u32>,
    /// papers run 的轮询间隔（秒），默认 21600（6 小时）。
    pub interval_secs: Option<u64>,
    /// digest 任务的执行底座，默认 codex。
    pub backend: Option<String>,
    /// 终态后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
}

impl ArxivSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 21_600;

    /// 由 [sources.arxiv] 解析出运行时配置；默认值只定义在这里。
    /// 通知路由由装配层按 [notify] 渠道注入，模块内不感知渠道知识。
    pub fn resolved(&self, notify: Option<NotifyRoute>) -> Result<PapersConfig> {
        Ok(PapersConfig {
            categories: self
                .categories
                .clone()
                .filter(|c| !c.is_empty())
                .context("sources.arxiv.categories required (e.g. [\"cs.CR\", \"cs.AI\"])")?,
            keywords: self.keywords.clone().unwrap_or_default(),
            interests: self.interests.clone().unwrap_or_default(),
            max_results: self.max_results.unwrap_or(100),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            backend: self.backend.clone().unwrap_or_else(|| "codex".into()),
            notify,
        })
    }
}
