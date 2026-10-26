//! 岗位筛选与跨轮合并：面灵首页只有最新 50 条（翻页要登录），
//! 所以每轮抓到的岗位合并进 vault 里的状态文件（.jobs-state.json），
//! 发布超过 30 天的条目自然过期——总览窗口不再受单页 50 条限制。

use crate::adapters::sources::jobs::mianling::MianlingJob;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 状态文件在 vault 里的位置（点开头，Obsidian 默认不显示）。
pub const STATE_PATH: &str = "05-Opportunities-机会/Internships-实习/.jobs-state.json";

/// 条目保留天数：发布超过这么久没再出现的岗位从总览里消失。
const KEEP_DAYS: i64 = 30;

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

/// 大湾区城市（标注 ⭐，不改排序）。
pub fn is_gba(city: &str) -> bool {
    city.contains("广州")
        || city.contains("深圳")
        || city.contains("南山区")
        || city.contains("番禺")
        || city.contains("珠海")
        || city.contains("东莞")
        || city.contains("佛山")
        || city.contains("香港")
        || city.contains("澳门")
}

/// 发布日期 "10月25日" → (月, 日)；无法解析返回 None。
pub fn posted_md(posted: &str) -> Option<(u32, u32)> {
    let re = regex::Regex::new(r"(\d{1,2})月(\d{1,2})日").expect("posted regex");
    let caps = re.captures(posted)?;
    Some((caps[1].parse().ok()?, caps[2].parse().ok()?))
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
}

/// 状态文件内容：key = 公司|岗位|城市。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct JobsState {
    pub entries: BTreeMap<String, JobEntry>,
}

/// 筛选一轮抓到的岗位：剔除非技术岗，保留实习+校招。
pub fn filter_jobs(jobs: Vec<MianlingJob>) -> Vec<JobEntry> {
    filter_entries(
        jobs.into_iter()
            .map(|job| JobEntry {
                company: job.company,
                role: job.role,
                city: job.city,
                audience: job.audience,
                kind: job.kind,
                posted: job.posted,
                platform: job.platform,
                jd: None,
                jd_tried: false,
            })
            .collect(),
    )
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

/// 把本轮岗位合并进状态：同 key 覆盖发布日等字段，但保留已抓到的 JD；
/// 过期条目清除。`today_md` 是当前 (月, 日)。
pub fn merge(state: &mut JobsState, fresh: Vec<JobEntry>, today_md: (u32, u32)) {
    for mut entry in fresh {
        let key = format!("{}|{}|{}", entry.company, entry.role, entry.city);
        if let Some(old) = state.entries.get(&key) {
            entry.jd = old.jd.clone();
            entry.jd_tried = old.jd_tried;
        }
        state.entries.insert(key, entry);
    }
    state.entries.retain(|_, entry| {
        match posted_md(&entry.posted) {
            Some(md) => day_diff(md, today_md) <= KEEP_DAYS,
            // 发布日未知（如腾讯青云计划）：保守保留。
            None => true,
        }
    });
}

/// 两个 (月,日) 相隔天数（0..=365，按同年近似，跨年取回绕最小值）。
fn day_diff(a: (u32, u32), b: (u32, u32)) -> i64 {
    let ord = |md: (u32, u32)| md.0 as i64 * 31 + md.1 as i64;
    let d = (ord(b) - ord(a)).abs();
    d.min((372 - d).abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(role: &str, kind: &str, posted: &str) -> MianlingJob {
        MianlingJob {
            company: "X".into(),
            role: role.into(),
            city: "北京".into(),
            audience: "未公布".into(),
            kind: kind.into(),
            posted: posted.into(),
            platform: None,
        }
    }

    #[test]
    fn keeps_tech_drops_nontech() {
        let fresh = filter_jobs(vec![
            job("AI研发实习生 - 模型数据工程", "实习", "10月25日"),
            job("Coding Agent 研发实习生", "实习", "10月22日"),
            job("大模型开发者生态运营实习生", "实习", "10月25日"),
            job("数据产品经理", "校招", "10月22日"),
            job("买手（广州）", "校招", "10月23日"),
            job("技术支持工程师", "校招", "10月21日"),
        ]);
        let roles: Vec<&str> = fresh.iter().map(|e| e.role.as_str()).collect();
        assert_eq!(roles, ["AI研发实习生 - 模型数据工程", "Coding Agent 研发实习生"]);
    }

    #[test]
    fn merge_refreshes_and_expires() {
        let mut state = JobsState::default();
        merge(&mut state, filter_jobs(vec![job("算法实习生", "实习", "10月25日")]), (10, 26));
        assert_eq!(state.entries.len(), 1);
        // 同 key 再来一条更新的发布日：覆盖
        merge(&mut state, filter_jobs(vec![job("算法实习生", "实习", "10月26日")]), (10, 26));
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries.values().next().unwrap().posted, "10月26日");
        // 推进 40 天：过期
        merge(&mut state, vec![], (12, 5));
        assert!(state.entries.is_empty());
    }
}
