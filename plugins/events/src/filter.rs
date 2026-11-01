//! 活动筛选与分类：
//! 方向闸——只留科技向（AI/网络安全/软件/开源/芯片/计算机等），化学生物材料类滤掉；
//! 地域分级——大湾区（粤港澳 11 城）⭐ 优先，线上不限地域；
//! 重大白名单——世界人工智能大会/GOSIM/CNCC 等全国级大会跨地域也通知。

use crate::source::EventItem;

/// 会议类关键词（命中即归为会议/论坛，否则视为直播/讲座）。
const CONFERENCE_KEYWORDS: &[&str] = &[
    "论坛", "会议", "大会", "峰会", "研讨会", "Meetup", "meetup", "Summit", "summit",
    "Symposium", "symposium", "Congress", "Workshop", "workshop", "沙龙", "CCF", "ICOMP",
    "Forum", "forum", "Conference", "conference",
];

/// 科技方向强命中词（标题命中即算相关）。
const TECH_KEYWORDS: &[&str] = &[
    "AI", "人工智能", "智能体", "大模型", "LLM", "机器学习", "深度学习", "神经网络",
    "PyTorch", "Transformer", "AGI", "AIGC", "具身", "机器人",
    "安全", "CTF", "攻防", "漏洞", "密码", "风控", "隐私计算",
    "软件", "开源", "计算", "数据", "操作系统", "数据库", "编程", "开发者", "程序员",
    "Rust", "C++", "Python", "Java", "云原生", "云计算", "边缘计算", "量子",
    "5G", "6G", "互联网", "IoT", "物联网",
    "黑客", "Hack", "hack", "Web3", "区块链", "自动驾驶", "智能汽车", "低空",
];

/// 明显非科技方向的排除词（命中且无强命中词时滤掉）。
const EXCLUDE_KEYWORDS: &[&str] = &[
    "化学", "聚合物", "高分子", "Polymer", "polymer", "Chemistry", "生物学", "生物医学",
    "医学", "药学", "材料", "地质", "农业", "食品", "环境科学", "大气", "海洋学",
];

/// 全国性/国际性重大大会白名单：即使不在大湾区也通知。
const MAJOR_KEYWORDS: &[&str] = &[
    "世界人工智能大会", "WAIC", "GOSIM", "世界互联网大会", "乌镇峰会",
    "云栖大会", "华为全联接", "HDC", "1024", "中国计算机大会", "CNCC",
    "进博会", "高交会", "世界5G大会", "全球开发者大会", "KubeCon",
];

/// 大湾区城市（粤港澳 11 城）。
const GBA_CITIES: &[&str] = &[
    "广州", "深圳", "香港", "澳门", "珠海", "佛山", "东莞", "惠州", "中山", "江门", "肇庆",
    "大湾区", "粤港澳", "南沙", "横琴", "前海", "河套",
];

#[derive(Clone, Debug)]
pub struct EventRow {
    pub id: String,
    pub title: String,
    pub start: i64,
    pub end: Option<i64>,
    /// 地点；线上直播为空时展示「线上」。
    pub place: String,
    /// 「会议/论坛」或「直播/讲座」。
    pub kind: &'static str,
    pub domains: Vec<String>,
    pub url: String,
    /// 大湾区线下。
    pub gba: bool,
    /// 全国/国际级重大大会（白名单命中）。
    pub major: bool,
}

pub fn classify(title: &str) -> &'static str {
    if CONFERENCE_KEYWORDS.iter().any(|k| title.contains(k)) {
        "会议/论坛"
    } else {
        "直播/讲座"
    }
}

/// 方向闸：强命中科技词 → 相关；命中排除词且无强命中 → 滤掉；
/// 其余看领域标签（人工智能/计算与存储/软件/半导体/无线/CCF 算相关）。
pub fn is_relevant(title: &str, domains: &[String]) -> bool {
    let strong = TECH_KEYWORDS.iter().any(|k| title.contains(k));
    if strong {
        return true;
    }
    if EXCLUDE_KEYWORDS.iter().any(|k| title.contains(k)) {
        return false;
    }
    domains.iter().any(|d| {
        ["人工智能", "计算与存储", "软件", "无线", "CCF", "信息"]
        .iter()
        .any(|k| d.contains(k))
    })
}

pub fn is_gba(place: &str) -> bool {
    GBA_CITIES.iter().any(|c| place.contains(c))
}

pub fn is_major(title: &str) -> bool {
    MAJOR_KEYWORDS.iter().any(|k| title.contains(k))
}

/// 是否值得推送通知：大湾区线下、或全国级重大大会（不论地域）。
pub fn worth_notify(row: &EventRow) -> bool {
    row.gba || row.major
}

/// 统一口径：科技方向 + 未结束（结束或开始时间未过 3 小时）+ id 去重 + 按开始时间升序。
/// 清洗：去掉「（备用线路）」的重复直播线路、标题尾部的 [topic:...] 导航残渣。
pub fn build_rows(items: Vec<EventItem>, now: i64) -> Vec<EventRow> {
    let mut seen = std::collections::HashSet::new();
    let mut seen_titles = std::collections::HashSet::new();
    let mut rows: Vec<EventRow> = items
        .into_iter()
        .filter(|item| item.end.unwrap_or(item.start) + 3 * 3600 >= now)
        .filter(|item| seen.insert(item.id.clone()))
        .map(|mut item| {
            item.title = clean_title(&item.title);
            item
        })
        // 备用线路与主线同题：同题只留第一条（接口顺序主线在前）。
        .filter(|item| seen_titles.insert(item.title.clone()))
        .filter(|item| is_relevant(&item.title, &item.domains))
        .map(|item| {
            let place = item.location.clone().unwrap_or_else(|| "线上".into());
            EventRow {
                gba: is_gba(&place),
                major: is_major(&item.title),
                kind: classify(&item.title),
                id: item.id,
                title: item.title,
                start: item.start,
                end: item.end,
                place,
                domains: item.domains,
                url: item.url,
            }
        })
        .collect();
    // 大湾区 ⭐ 优先，其次按开始时间。
    rows.sort_by_key(|row| (!row.gba, row.start));
    rows
}

/// 标题清洗：去「（备用线路）」前缀与 `[topic:...]` 后缀残渣。
fn clean_title(title: &str) -> String {
    let t = title
        .trim_start_matches("（备用线路）")
        .trim_start_matches("(备用线路)")
        .trim();
    match t.find("[topic:") {
        Some(index) => t[..index].trim().to_owned(),
        None => t.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str, start: i64) -> EventItem {
        EventItem {
            id: id.into(),
            title: title.into(),
            start,
            end: None,
            location: None,
            url: "https://www.chaspark.com".into(),
            domains: Vec::new(),
        }
    }

    #[test]
    fn conference_keywords_classify() {
        assert_eq!(classify("大湾区科学论坛"), "会议/论坛");
        assert_eq!(classify("openUBMC Meetup 深圳站"), "会议/论坛");
        assert_eq!(classify("大模型推理的胜负手是调度"), "直播/讲座");
    }

    #[test]
    fn direction_gate_filters_chemistry_keeps_ai_and_security() {
        assert!(is_relevant("2026 CCF软件大会——软件工程智能体创新论坛", &[]));
        assert!(is_relevant("网络安全攻防研讨会", &[]));
        assert!(is_relevant("GOSIM AI 开源论坛", &[]));
        assert!(!is_relevant("北京大学无机化学论坛", &[]));
        assert!(!is_relevant(
            "International Frontiers in Polymer Science 2026",
            &[]
        ));
        // 无强词但领域标签是人工智能 → 相关
        assert!(is_relevant("某讲座", &["人工智能领域".to_string()]));
        // 化学标题 + AI 强词 → 保留（AI4S 交叉）
        assert!(is_relevant("AI for Science：大模型加速化学发现", &[]));
    }

    #[test]
    fn geo_and_major_rules() {
        assert!(is_gba("广州南沙"));
        assert!(is_gba("深圳"));
        assert!(!is_gba("线上"));
        assert!(!is_gba("北京"));
        assert!(is_major("世界人工智能大会 WAIC 2026"));
        assert!(is_major("GOSIM 2026"));
        assert!(!is_major("某高校讲座"));
    }

    #[test]
    fn build_rows_filters_past_dedupes_and_sorts_gba_first() {
        let now = 1_793_600_000;
        let mut gz = item("a", "广州 AI 峰会", now + 7200);
        gz.location = Some("广州".into());
        let rows = build_rows(
            vec![
                item("x", "无机化学论坛", now + 100),
                item("b", "昨天讲座 AI", now - 10 * 3600),
                item("c", "大模型线上分享", now + 3600),
                gz,
            ],
            now,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "a"); // 大湾区优先于开始时间
        assert!(rows[0].gba);
        assert!(worth_notify(&rows[0]));
        assert!(!worth_notify(&rows[1])); // 线上非重大不通知
    }
}
