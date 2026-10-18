use serde::Deserialize;

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
}
