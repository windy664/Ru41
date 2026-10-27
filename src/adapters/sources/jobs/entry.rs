//! 岗位条目与技术岗筛选口径：适配器层的数据契约。
//! 各来源（面灵 / BOSS sidecar）都归一成 `JobEntry` 并套用同一套口径；
//! modules/jobs 的状态合并与总览渲染复用这里的类型（依赖方向 modules → adapters）。

use serde::{Deserialize, Serialize};

/// 技术向保留词（命中即留）。
fn keep_re() -> regex::Regex {
    regex::Regex::new(
        r"(?i)AI|大模型|Agent|智能体|算法|后端|全栈|安全|Infra|数据|研发|推理|模型|LLM|Coding|前端|嵌入式|运维|开发|工程师|Intern|Engineer",
    )
    .expect("keep regex")
}

/// 非技术剔除词（命中即丢，优先级高于保留词——"AI产品经理"这类擦边岗不要）。
fn drop_re() -> regex::Regex {
    regex::Regex::new(r"产品|运营|商业分析|买手|销售|客服|维养|行政|人力|市场|解决方案|技术支持")
        .expect("drop regex")
}

/// 合并后的总览条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobEntry {
    pub company: String,
    pub role: String,
    pub city: String,
    pub audience: String,
    /// "实习" / "校招"。
    pub kind: String,
    pub posted: String,
    /// 回源平台（ashby/greenhouse/...），老状态文件没有此字段。
    #[serde(default)]
    pub platform: Option<String>,
    /// JD 纯文本（回源官方招聘站抓到才有）。
    #[serde(default)]
    pub jd: Option<String>,
    /// 已尝试过 JD 抓取（无论成败），避免每轮反复打官方 API。
    #[serde(default)]
    pub jd_tried: bool,
    /// 官方岗位页链接（BOSS 由 encryptJobId 构造；ashby/greenhouse/smartrecruiters
    /// 回源抓 JD 命中时一并带出；面灵无公开详情页，国内平台暂缺）。
    #[serde(default)]
    pub link: Option<String>,
    /// 已尝试过链接补全（无论成败），避免每轮反复打官方 API。
    #[serde(default)]
    pub link_tried: bool,
}

/// 对已构造好的条目（如 BOSS sidecar 结果）套用同一套技术岗口径。
pub fn filter_entries(entries: Vec<JobEntry>) -> Vec<JobEntry> {
    let keep = keep_re();
    let drop = drop_re();
    entries
        .into_iter()
        .filter(|e| !drop.is_match(&e.role) && keep.is_match(&e.role))
        .collect()
}
