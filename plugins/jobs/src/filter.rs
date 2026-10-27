//! 岗位筛选与跨轮合并：面灵首页只有最新 50 条（翻页要登录），
//! 所以每轮抓到的岗位合并进 vault 里的状态文件（.jobs-state.json），
//! 发布超过 30 天的条目自然过期——总览窗口不再受单页 50 条限制。

use ru41::adapters::sources::jobs::mianling::MianlingJob;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 条目类型与技术岗口径在适配器层定义（`adapters::sources::jobs::entry`），
/// 此处 re-export 保持模块内既有引用路径不变。
pub use ru41::adapters::sources::jobs::entry::{JobEntry, filter_entries};

/// 状态文件在 vault 里的位置（点开头，Obsidian 默认不显示）。
pub const STATE_PATH: &str = "05-Opportunities-机会/Internships-实习/.jobs-state.json";

/// 条目保留天数：发布超过这么久没再出现的岗位从总览里消失。
const KEEP_DAYS: i64 = 30;

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
                link: None,
                link_tried: false,
            })
            .collect(),
    )
}

/// 把本轮岗位合并进状态：同 key 覆盖发布日等字段，但保留已抓到的 JD；
/// 过期条目清除。`today_md` 是当前 (月, 日)。
pub fn merge(state: &mut JobsState, fresh: Vec<JobEntry>, today_md: (u32, u32)) {
    for mut entry in fresh {
        let key = format!("{}|{}|{}", entry.company, entry.role, entry.city);
        if let Some(old) = state.entries.get(&key) {
            entry.jd = old.jd.clone();
            entry.jd_tried = old.jd_tried;
            // 链接同理：新抓的条目没有就继承旧的（BOSS 条目自带，面灵条目靠 JD 回源补）。
            if entry.link.is_none() {
                entry.link = old.link.clone();
            }
            entry.link_tried = old.link_tried || entry.link.is_some();
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
    use crate::overview::render;
    use ru41::adapters::sources::jobs::{entry::JobEntry, mianling::MianlingJob};

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
        assert_eq!(
            roles,
            ["AI研发实习生 - 模型数据工程", "Coding Agent 研发实习生"]
        );
    }

    #[test]
    fn merge_refreshes_and_expires() {
        let mut state = JobsState::default();
        merge(
            &mut state,
            filter_jobs(vec![job("算法实习生", "实习", "10月25日")]),
            (10, 26),
        );
        assert_eq!(state.entries.len(), 1);
        // 同 key 再来一条更新的发布日：覆盖
        merge(
            &mut state,
            filter_jobs(vec![job("算法实习生", "实习", "10月26日")]),
            (10, 26),
        );
        assert_eq!(state.entries.len(), 1);
        assert_eq!(state.entries.values().next().unwrap().posted, "10月26日");
        // 推进 40 天：过期
        merge(&mut state, vec![], (12, 5));
        assert!(state.entries.is_empty());
    }
}
