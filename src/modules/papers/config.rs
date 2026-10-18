use serde::Deserialize;

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
