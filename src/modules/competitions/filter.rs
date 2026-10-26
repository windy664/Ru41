//! 竞赛筛选口径（纯函数，全部可单测）：
//! 必须有奖（现金/奖品/权益，纯证书不要）× 地理只留大湾区线下/国内线上/国外线上；
//! 剔除中小学生赛与已截止；同名跨源去重（保留信息更全的）。

use crate::adapters::sources::competition::devpost::{DevpostHackathon, parse_submission_end};
use crate::adapters::sources::competition::hack123::Hack123Event;
use crate::adapters::sources::competition::hackertrip::HackertripEvent;
use crate::adapters::sources::competition::wqai::WqaiRace;
use chrono::{Datelike, NaiveDate};

/// 总览表格的一行。
#[derive(Debug, Clone)]
pub struct CompetitionRow {
    pub deadline: Option<NaiveDate>,
    pub name: String,
    pub place: String,
    pub prize: String,
    pub start: Option<NaiveDate>,
    pub link: String,
    pub source: &'static str,
    /// 题材/赛道（hackertrip tracks、devpost themes；wqai 没有）。
    pub themes: Vec<String>,
    pub gba: bool,
}

/// 大湾区城市（含港澳）。
const GBA_CITIES: &[&str] = &[
    "广州",
    "深圳",
    "珠海",
    "佛山",
    "东莞",
    "中山",
    "江门",
    "惠州",
    "肇庆",
    "香港",
    "澳门",
    "Hong Kong",
];

/// 有奖判定：现金/奖品/权益类关键词；纯证书/纯头衔不算。
fn has_prize(text: &str) -> bool {
    let re = regex::Regex::new(r"奖金|奖池|现金|万元|￥|¥|\$|奖品|权益|投资|兑换券|实物|里数|机票")
        .expect("prize regex");
    !text.is_empty() && re.is_match(text)
}

/// 中小学生赛判定：名称直接点名，或受众描述含"面向小学/初中/高中""4-12 年级"
/// "aged 14–18""high school"（hack123 的受众段）等未成年信号。
fn is_k12(name: &str, audience: &str) -> bool {
    let name_re = regex::Regex::new(r"青少年|少年|中小学|K-?12|中学生|高中生|(?i)high school")
        .expect("k12 name regex");
    if name_re.is_match(name) {
        return true;
    }
    let aud_re = regex::Regex::new(
        r"面向[^。；]{0,12}(小学|初中|高中)|4\s*[-–至]\s*12\s*年级|(?i)aged\s+1[2-8]",
    )
    .expect("k12 audience regex");
    aud_re.is_match(&format!("{name} {audience}"))
}

/// 地理分类：大湾区线下 / 线上 / 丢弃（非大湾区线下）。
#[derive(Debug, PartialEq, Eq)]
enum Geo {
    Gba,
    Online,
    Drop,
}

fn geo_class(place: &str, online: bool) -> Geo {
    if GBA_CITIES.iter().any(|c| place.contains(c)) {
        return Geo::Gba;
    }
    if online || place.contains("线上") {
        return Geo::Online;
    }
    Geo::Drop
}

/// 未来判定：报名截止或开赛在今天及以后（今天截止的保留——行动窗口最后一天）。
fn is_future(row: &CompetitionRow, today: NaiveDate) -> bool {
    row.deadline
        .or(row.start)
        .is_some_and(|d| d >= today.pred_opt().unwrap_or(today))
}

/// 去重键：剥掉年份/标点/空格后的核心名前 10 字。
fn core_name(name: &str) -> String {
    let no_year = regex::Regex::new(r"20\d{2}")
        .expect("year regex")
        .replace_all(name, "");
    let re = regex::Regex::new(r"[\s·\-—（）()【】「」\[\]]").expect("punct regex");
    re.replace_all(&no_year, "").chars().take(10).collect()
}

fn row_score(row: &CompetitionRow) -> u8 {
    !row.prize.is_empty() as u8 + row.deadline.is_some() as u8 + (row.source == "HackerTrip") as u8
}

/// 全量入口：逐条应用口径 → 未来过滤 → 去重 → 大湾区优先 + 截止日升序。
pub fn build_rows(input: Vec<CompetitionRow>, today: NaiveDate) -> Vec<CompetitionRow> {
    let mut rows: Vec<CompetitionRow> = input
        .into_iter()
        .filter(|r| {
            !r.prize.is_empty()
                && has_prize(&r.prize)
                && !is_k12(&r.name, "")
                && is_future(r, today)
        })
        .collect();
    // 去重：同名保留字段更全的
    let mut seen: std::collections::HashMap<String, CompetitionRow> = Default::default();
    for row in rows.drain(..) {
        let key = core_name(&row.name);
        let key = if key.is_empty() {
            row.name.clone()
        } else {
            key
        };
        match seen.get(&key) {
            Some(old) if row_score(old) >= row_score(&row) => {}
            _ => {
                seen.insert(key, row);
            }
        }
    }
    let mut rows: Vec<CompetitionRow> = seen.into_values().collect();
    rows.sort_by(|a, b| {
        (!a.gba)
            .cmp(&!b.gba)
            .then(a.deadline.or(a.start).cmp(&b.deadline.or(b.start)))
    });
    rows
}

/// K12 判定暴露在行构建前需要受众文本；这里给 hackertrip 单独用的入口。
fn keep(row: CompetitionRow, geo: Geo, k12: bool) -> Option<CompetitionRow> {
    if k12 || geo == Geo::Drop || !has_prize(&row.prize) {
        return None;
    }
    Some(CompetitionRow {
        gba: geo == Geo::Gba,
        ..row
    })
}

impl From<&HackertripEvent> for CompetitionRow {
    fn from(ev: &HackertripEvent) -> Self {
        let mode_cn = match ev.mode.as_str() {
            "online" => "线上",
            "offline" => "线下",
            "hybrid" => "混合",
            _ => "",
        };
        let mut themes = vec![];
        if !ev.theme.is_empty() {
            themes.push(crate::util::truncate_chars(&ev.theme, 30));
        }
        themes.extend(
            ev.tracks
                .iter()
                .take(4)
                .map(|t| crate::util::truncate_chars(t, 20)),
        );
        CompetitionRow {
            deadline: NaiveDate::parse_from_str(&ev.registration_deadline, "%Y-%m-%d").ok(),
            name: ev.name.clone(),
            place: format!("{}·{mode_cn}", ev.city)
                .trim_end_matches('·')
                .to_owned(),
            prize: ev.prize_pool.chars().take(40).collect(),
            start: NaiveDate::parse_from_str(&ev.start_date, "%Y-%m-%d").ok(),
            link: ev.website.clone(),
            source: "HackerTrip",
            themes,
            gba: false,
        }
    }
}

impl From<&DevpostHackathon> for CompetitionRow {
    fn from(h: &DevpostHackathon) -> Self {
        let prize_text = h.prize_text();
        let amount: String = prize_text.chars().filter(|c| c.is_ascii_digit()).collect();
        let prize = if amount.parse::<u64>().unwrap_or(0) > 0 {
            prize_text.clone()
        } else {
            String::new()
        };
        CompetitionRow {
            deadline: h
                .submission_period_dates
                .as_deref()
                .and_then(parse_submission_end),
            name: h.title.clone(),
            place: "线上".to_owned(),
            prize,
            start: None,
            link: h.url.clone(),
            source: "Devpost",
            themes: h.theme_names(),
            gba: false,
        }
    }
}

/// wqai 截止列年份推断："10-28" → 今年（月份已过则明年）；"2026-10-26" 直接用。
fn wqai_deadline(raw: &str, today: NaiveDate) -> Option<NaiveDate> {
    let short = regex::Regex::new(r"^(\d{1,2})-(\d{1,2})$").expect("short date regex");
    if let Some(caps) = short.captures(raw) {
        let month: u32 = caps[1].parse().ok()?;
        let day: u32 = caps[2].parse().ok()?;
        let year = if month >= today.month() {
            today.year()
        } else {
            today.year() + 1
        };
        return NaiveDate::from_ymd_opt(year, month, day);
    }
    let full = regex::Regex::new(r"^(20\d{2})-(\d{1,2})-(\d{1,2})").expect("full date regex");
    let caps = full.captures(raw)?;
    NaiveDate::from_ymd_opt(
        caps[1].parse().ok()?,
        caps[2].parse().ok()?,
        caps[3].parse().ok()?,
    )
}

/// wqai 行 → CompetitionRow（带今天做年份推断）。
pub fn from_wqai(race: WqaiRace, today: NaiveDate) -> CompetitionRow {
    CompetitionRow {
        deadline: wqai_deadline(&race.deadline_raw, today),
        name: race.title,
        place: race.venue,
        prize: race.rewards,
        start: None,
        link: race.link,
        source: "wqai",
        themes: vec![],
        gba: false,
    }
}

/// 各源条目收口：geo/k12/prize 判定在这里统一完成（区别于 build_rows 的全量后处理，
/// 这两个入口处理需要源侧额外信息的判定：k12 受众文本、devpost 的 online 标志）。
pub fn collect_hackertrip(ev: HackertripEvent) -> Option<CompetitionRow> {
    let audience = ev.tracks.join(" ");
    let k12 = is_k12(&ev.name, &audience) || is_k12(&ev.name, &ev.theme);
    let row = CompetitionRow::from(&ev);
    let geo = geo_class(&row.place, ev.mode == "online");
    keep(row, geo, k12)
}

pub fn collect_devpost(h: &DevpostHackathon) -> Option<CompetitionRow> {
    if !matches!(h.open_state.as_deref(), Some("open") | Some("upcoming")) || !h.is_online() {
        return None;
    }
    let row = CompetitionRow::from(h);
    let geo = geo_class(&row.place, true);
    keep(row, geo, false)
}

pub fn collect_wqai(race: WqaiRace, today: NaiveDate) -> Option<CompetitionRow> {
    let k12 = is_k12(&race.title, &race.team);
    let row = from_wqai(race, today);
    let geo = geo_class(&row.place, false);
    keep(row, geo, k12)
}

impl From<&Hack123Event> for CompetitionRow {
    fn from(ev: &Hack123Event) -> Self {
        CompetitionRow {
            deadline: NaiveDate::parse_from_str(&ev.deadline, "%Y-%m-%d").ok(),
            name: ev.name.clone(),
            place: if ev.is_online() {
                "线上".to_owned()
            } else {
                ev.place.clone()
            },
            prize: ev.prize.trim_start_matches("prize ").to_owned(),
            start: NaiveDate::parse_from_str(&ev.starts, "%Y-%m-%d").ok(),
            link: ev.link.clone(),
            source: "hack123",
            themes: if ev.themes.is_empty() {
                vec![]
            } else {
                vec![ev.themes.clone()]
            },
            gba: false,
        }
    }
}

pub fn collect_hack123(ev: &Hack123Event) -> Option<CompetitionRow> {
    let k12 = is_k12(&ev.name, &ev.audience);
    let row = CompetitionRow::from(ev);
    let geo = geo_class(&row.place, ev.is_online());
    keep(row, geo, k12)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, prize: &str, place: &str, deadline: Option<NaiveDate>) -> CompetitionRow {
        CompetitionRow {
            deadline,
            name: name.into(),
            place: place.into(),
            prize: prize.into(),
            start: None,
            link: String::new(),
            source: "wqai",
            themes: vec![],
            gba: false,
        }
    }

    #[test]
    fn prize_rule_drops_certificates() {
        assert!(has_prize("总奖金 74,000 元"));
        assert!(has_prize("$67,500"));
        assert!(has_prize("总价值 150 万资源权益（非现金）"));
        assert!(!has_prize("初赛一二三等奖电子证书"));
        assert!(!has_prize("未标注"));
        assert!(!has_prize(""));
    }

    #[test]
    fn geo_rule_keeps_gba_and_online() {
        assert_eq!(geo_class("深圳·线下", false), Geo::Gba);
        assert_eq!(geo_class("香港（主场）", false), Geo::Gba);
        assert_eq!(geo_class("线上（全国）", false), Geo::Online);
        assert_eq!(geo_class("上海", true), Geo::Online);
        assert_eq!(geo_class("苏州·混合", false), Geo::Drop);
        assert_eq!(geo_class("美国 · 伯克利", false), Geo::Drop);
    }

    #[test]
    fn k12_rule() {
        assert!(is_k12("粤创杯中小学生 AI 应用创作大赛", ""));
        assert!(is_k12("AIY HACK LEAGUE", "面向小学、初中与高中"));
        assert!(!is_k12(
            "AI For Good公益黑客松",
            "养老照护、儿童与青少年议题"
        ));
    }

    #[test]
    fn wqai_year_inference_rolls_forward_only_when_month_passed() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        assert_eq!(
            wqai_deadline("11-15", today),
            NaiveDate::from_ymd_opt(2026, 11, 15)
        );
        // 9 月在今年 10 月之前：原文是"已截止"行的旧日期，不滚到明年
        // （状态列已把已截止行过滤，这里只是兜底语义）
        assert_eq!(
            wqai_deadline("09-07", today),
            NaiveDate::from_ymd_opt(2027, 9, 7)
        );
        assert_eq!(
            wqai_deadline("2026-10-30", today),
            NaiveDate::from_ymd_opt(2026, 10, 30)
        );
    }

    #[test]
    fn build_rows_dedupes_by_core_name_and_sorts() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let rows = vec![
            row("雾客松 2027（WAKEATHON）", "奖池 20 万元", "重庆", None),
            row(
                "雾客松 WAKEATHON",
                "奖池 20 万元",
                "线下",
                NaiveDate::from_ymd_opt(2027, 1, 10),
            ),
        ];
        let out = build_rows(rows, today);
        assert_eq!(out.len(), 1, "同名跨源去重");
        assert!(out[0].deadline.is_some(), "保留有截止日期的版本");
    }

    #[test]
    fn build_rows_drops_past() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let rows = vec![row(
            "旧赛",
            "奖金 1 万元",
            "线上",
            NaiveDate::from_ymd_opt(2026, 10, 20),
        )];
        assert!(build_rows(rows, today).is_empty());
    }
}
