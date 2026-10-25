use crate::domain::NotifyRoute;
use anyhow::{Context, Result};
use serde::Deserialize;

use super::service::RssConfig;

/// RSS 信息流追踪配置；rss poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RssSection {
    /// 订阅的 RSS 2.0 feed 地址列表。
    pub feeds: Option<Vec<String>>,
    /// 关键词粗筛；空或缺省表示不过滤。
    pub keywords: Option<Vec<String>>,
    /// 个人关注方向描述，注入 digest prompt 供 Agent 判断相关性。
    pub interests: Option<String>,
    /// 每个源单次最多处理的条目数，默认 50。
    pub max_items_per_feed: Option<u32>,
    /// rss run 的轮询间隔（秒），默认 21600（6 小时）。
    pub interval_secs: Option<u64>,
    /// digest 任务的执行底座，默认 codex。
    pub backend: Option<String>,
    /// 终态后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// digest 模式："realtime"（默认，有候选即精选）或 "daily"（攒候选池，
    /// 到点出日报）。daily 用少量 GLM 调用换固定节奏的高信噪比推送。
    pub digest_mode: Option<String>,
    /// daily 模式晚报时刻（本地时，0-23），默认 21。
    pub evening_hour: Option<u32>,
    /// daily 模式早报时刻（本地时，0-23）；缺省不出早报。
    pub morning_hour: Option<u32>,
    /// 条目最大年龄（天），更旧的直接丢弃；默认 2。
    pub max_age_days: Option<u64>,
    /// daily 模式入池前是否跑关键词粗筛；默认 true。
    /// false 时近时效新条目全量入池（封顶 80），相关性判断完全交给 digest。
    pub pool_keyword_filter: Option<bool>,
}

impl RssSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 21_600;

    /// 由 [sources.rss] 解析出运行时配置；默认值只定义在这里。
    /// 通知路由由装配层按 [notify] 渠道注入，模块内不感知渠道知识。
    pub fn resolved(&self, notify: Option<NotifyRoute>) -> Result<RssConfig> {
        Ok(RssConfig {
            feeds: self
                .feeds
                .clone()
                .filter(|f| !f.is_empty())
                .context("sources.rss.feeds required")?,
            keywords: self.keywords.clone().unwrap_or_default(),
            interests: self.interests.clone().unwrap_or_default(),
            max_items_per_feed: self.max_items_per_feed.unwrap_or(50),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            backend: self.backend.clone().unwrap_or_else(|| "codex".into()),
            notify,
            digest_mode: super::service::DigestMode::parse(self.digest_mode.as_deref())?,
            evening_hour: self.evening_hour.unwrap_or(21),
            morning_hour: self.morning_hour,
            max_age_days: self.max_age_days.unwrap_or(2),
            pool_keyword_filter: self.pool_keyword_filter.unwrap_or(true),
        })
    }
}
