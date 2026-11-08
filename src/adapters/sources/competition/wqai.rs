//! 湾区 AI 日历（wqai.ltd）：粤港澳大湾区活动与赛事聚合站。
//! 两类接入：赛事页（calendar/race/）解析静态表格；官方公共 ICS 订阅源
//! （calendar/calendar.ics， llms.txt 公示、无需订阅码）解析 VEVENT，
//! 覆盖活动+赛事，UID 天然去重，含城市/场馆/票务等富描述。
//! 在 rss 模块里以 `wqai+<url>` 前缀接入，产出与 RSS 条目同构的 FeedItem 进候选池。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::Result;

/// rss feed 列表里的接入前缀：
/// `wqai+https://wqai.ltd/calendar/race/index.html`（赛事表格）
/// `wqai+https://wqai.ltd/calendar/calendar.ics`（大湾区活动日历 ICS）。
pub const URL_PREFIX: &str = "wqai+";

/// 注册表条目：`wqai+<url>` 路由到本源的赛事表格 / ICS 解析。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { WqaiClient::new().fetch(&url).await }),
};

pub struct WqaiClient {
    http: reqwest::Client,
}

impl Default for WqaiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WqaiClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 按 URL 形态分发：.ics 走 iCalendar 解析，其余按赛事表格页处理。
    pub async fn fetch(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(self.http.get(url), "wqai").await?;
        if url.ends_with(".ics") {
            parse_ics(&body)
        } else {
            parse_race_page(&body, url)
        }
    }

    /// 竞赛日程总览用的类型化入口（仅赛事表格页，丢弃状态列）。
    /// 列表卡片没有奖励字段，逐个详情页补（meta description 带「奖励：…」）。
    pub async fn fetch_races(&self, url: &str) -> Result<Vec<WqaiRace>> {
        let body = fetch_text(self.http.get(url), "wqai").await?;
        let mut races: Vec<WqaiRace> = parse_race_rows(&body, url)
            .into_iter()
            .map(|(race, _)| race)
            .collect();
        for race in &mut races {
            let Ok(detail) = fetch_text(self.http.get(&race.link), "wqai-detail").await else {
                continue;
            };
            let Some(meta) = capture(&detail, r#"<meta name="description" content="([^"]+)""#)
            else {
                continue;
            };
            if let Some(rewards) = meta.split("奖励：").nth(1) {
                race.rewards = rewards
                    .trim()
                    .trim_end_matches('。')
                    .chars()
                    .take(40)
                    .collect();
            }
            if let Some(d) = capture(&meta, r"报名截止\s*(\d{1,2}-\d{1,2})") {
                race.deadline_raw = d;
            }
        }
        Ok(races)
    }
}

/// 解析 iCalendar：展开折行 → 切 VEVENT → 提取字段。
/// 只保留未结束（DTEND >= 今天，东八区）的条目。
fn parse_ics(raw: &str) -> Result<Vec<FeedItem>> {
    // 折行展开：以空格/制表符开头的行是上一行的延续（RFC 5545）。
    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        if let Some(stripped) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t'))
            && let Some(last) = lines.last_mut()
        {
            last.push_str(stripped);
            continue;
        }
        lines.push(line.trim_end().to_owned());
    }
    let today = {
        // 东八区今天 00:00 的 unix 时间戳
        let now = crate::util::now() + 8 * 3600;
        (now / 86_400) * 86_400 - 8 * 3600
    };
    let mut items = Vec::new();
    let mut in_event = false;
    let mut fields: Vec<&str> = Vec::new();
    let flush = |fields: &[&str], items: &mut Vec<FeedItem>| {
        if let Some(item) = vevent_to_item(fields, today) {
            items.push(item);
        }
    };
    for line in &lines {
        match line.as_str() {
            "BEGIN:VEVENT" => {
                in_event = true;
                fields.clear();
            }
            "END:VEVENT" => {
                if in_event {
                    flush(&fields, &mut items);
                }
                in_event = false;
            }
            _ if in_event => fields.push(line.as_str()),
            _ => {}
        }
    }
    Ok(items)
}

fn vevent_to_item(fields: &[&str], today: i64) -> Option<FeedItem> {
    let get = |name: &str| -> Option<String> {
        fields
            .iter()
            .filter_map(|line| line.split_once(':'))
            .find(|(head, _)| head.split(';').next() == Some(name))
            .map(|(_, v)| ics_unescape(v))
    };
    let uid = get("UID")?;
    let title = get("SUMMARY")?;
    let end_ts = ics_ts(&get("DTEND")?).or_else(|| ics_ts(&get("DTSTART")?))?;
    if end_ts < today {
        return None;
    }
    let start = get("DTSTART").unwrap_or_default();
    let location = get("LOCATION").unwrap_or_default();
    let categories = get("CATEGORIES").unwrap_or_default();
    let mut summary_parts = vec![];
    if !categories.is_empty() {
        summary_parts.push(categories.replace(',', " · "));
    }
    if !location.is_empty() {
        summary_parts.push(location);
    }
    if let Some(end) = get("DTEND")
        && let (Some(s), Some(e)) = (ics_date_str(&start), ics_date_str(&end))
    {
        summary_parts.push(if s == e { s } else { format!("{s} ~ {e}") });
    }
    Some(FeedItem {
        id: format!("wqai-{}", uid.split('@').next().unwrap_or(&uid)),
        title,
        link: get("URL").unwrap_or_else(|| "https://wqai.ltd/".into()),
        summary: summary_parts.join(" · "),
        published: ics_date_str(&start).unwrap_or_default(),
    })
}

/// ICS 转义还原：\, \n \\ \;
fn ics_unescape(raw: &str) -> String {
    raw.replace("\\n", " ")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
        .trim()
        .to_owned()
}

/// "20261016" / "20261016T133000" / 带 Z 后缀 → unix 时间戳（东八区口径）
fn ics_ts(raw: &str) -> Option<i64> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    let (date, time) = if digits.len() >= 8 {
        let (d, t) = digits.split_at(8);
        (d, t.get(..6).unwrap_or("000000"))
    } else {
        return None;
    };
    let naive =
        chrono::NaiveDateTime::parse_from_str(&format!("{date}{time}"), "%Y%m%d%H%M%S").ok()?;
    // 带 Z 的是 UTC，否则按东八区
    let offset = if raw.ends_with('Z') { 0 } else { 8 * 3600 };
    Some(naive.and_utc().timestamp() - offset)
}

/// "20261016..." → "2026-10-16"
fn ics_date_str(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &digits[0..4],
        &digits[4..6],
        &digits[6..8]
    ))
}

/// 赛事页表格的类型化行：雷达映射成 FeedItem，竞赛日程总览直接用字段。
pub struct WqaiRace {
    pub id: String,
    pub title: String,
    pub link: String,
    /// 截止列原文（"10-28" / "2026-10-26" 等），年份推断在调用方做。
    pub deadline_raw: String,
    pub venue: String,
    pub team: String,
    pub rewards: String,
}

impl WqaiRace {
    fn to_feed_item(&self, status: &str) -> FeedItem {
        FeedItem {
            id: self.id.clone(),
            title: self.title.clone(),
            link: self.link.clone(),
            summary: format!(
                "截止 {} · {} · {} · 奖励：{} · 状态：{status}",
                self.deadline_raw, self.venue, self.team, self.rewards
            ),
            published: String::new(),
        }
    }
}

/// 解析赛事页卡片（2026-11 站方改版后的结构）：
/// <div class="daygroup"><div class="dayhead"><span class="d">11.10</span>…
/// <a class="ev race" href="../../calendar/race/race-rNNN.html" …>
///   <div><div class="tt">标题</div><div class="mt"><i>城市</i>…</div>
///   <div class="st on">报名中 / 即将截止 · 还剩 N 天</div></a>
/// dayhead 的日期即报名截止（MM.DD → MM-DD）；只取 `st on`（开放中）的
/// 条目，已截止/赛程进行中丢弃。奖励列表页没有，由 fetch_races 补详情页。
pub fn parse_race_rows(html: &str, _page_url: &str) -> Vec<(WqaiRace, String)> {
    let mut rows = Vec::new();
    for group in html.split("<div class=\"daygroup\"").skip(1) {
        let Some(deadline) = capture(group, r#"<span class="d">(\d{1,2}\.\d{1,2})</span>"#)
            .map(|d| d.replace('.', "-"))
        else {
            continue;
        };
        for card in group.split("<a class=\"ev race\" ").skip(1) {
            if let Some(race) = parse_card(card, &deadline) {
                rows.push(race);
            }
        }
    }
    rows
}

fn parse_card(card: &str, deadline: &str) -> Option<(WqaiRace, String)> {
    let href = capture(card, r#"href="[^"]*(race-r\d+\.html)""#)?;
    let title = capture(card, r#"<div class="tt">([^<]+)</div>"#)?;
    let venue = capture(card, r#"<div class="mt"><i>([^<]*)</i>"#).unwrap_or_default();
    // 开放中才是 st on；已截止/赛程进行中是裸 st。
    if capture(card, r#"<div class="(st[^"]*)">"#).as_deref() != Some("st on") {
        return None;
    }
    let status = capture(card, r#"<div class="st[^"]*">([^<]+)</div>"#).unwrap_or_default();
    Some((
        WqaiRace {
            id: href.trim_end_matches(".html").to_owned(),
            title,
            link: format!("https://wqai.ltd/calendar/race/{href}"),
            deadline_raw: deadline.to_owned(),
            venue,
            team: String::new(),
            rewards: String::new(),
        },
        status,
    ))
}

/// 第一个捕获组；无匹配或空串返回 None。
fn capture(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    let value = re.captures(text)?.get(1)?.as_str().trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// 解析赛事页为 FeedItem（雷达入口）。
fn parse_race_page(html: &str, page_url: &str) -> Result<Vec<FeedItem>> {
    Ok(parse_race_rows(html, page_url)
        .into_iter()
        .map(|(race, status)| race.to_feed_item(&status))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
<div class="rows" id="listBox">
<div class="daygroup" style="--n:1"><div class="dayhead"><span class="d">11.10</span><span class="w">周二</span><span class="rule"></span><span class="w n-in">2 项</span></div>
<a class="ev race" href="../../calendar/race/race-r063.html" data-city="zh" data-hay="2026 首届珠海人工智能 OPC 创业大赛 珠海 "><div class="th"><img src="x.webp"></div><div><div class="tt">2026 首届珠海人工智能 OPC 创业大赛</div><div class="mt"><i>珠海</i><span class="k">赛事</span></div></div><div class="st on">即将截止 · 还剩 2 天</div></a>
<a class="ev race" href="../../calendar/race/race-r070.html" data-city="on" data-hay="PayPal AI Hackathon 2026 线上 "><div class="th"></div><div><div class="tt">PayPal AI Hackathon 2026</div><div class="mt"><i>线上</i><span class="k">赛事</span></div></div><div class="st on">报名中</div></a></div>
<div class="daygroup" style="--n:1"><div class="dayhead"><span class="d">11.13</span><span class="w">周五</span><span class="rule"></span><span class="w n-in">1 项</span></div>
<a class="ev race" href="../../calendar/race/race-r071.html" data-city="zh"><div><div class="tt">已截止的旧赛</div><div class="mt"><i>广州</i></div></div><div class="st">报名已截止</div></a></div>
</div>"#;

    #[test]
    fn parses_race_rows_with_stable_ids() {
        let items = parse_race_page(FIXTURE, "https://wqai.ltd/calendar/race/index.html").unwrap();
        assert_eq!(items.len(), 2, "已截止条目被跳过");
        assert_eq!(items[0].id, "race-r063");
        assert_eq!(items[0].title, "2026 首届珠海人工智能 OPC 创业大赛");
        assert_eq!(
            items[0].link,
            "https://wqai.ltd/calendar/race/race-r063.html"
        );
        assert!(items[0].summary.contains("截止 11-10"));
        assert!(items[0].summary.contains("珠海"));
        assert!(items[0].summary.contains("即将截止 · 还剩 2 天"));
        assert_eq!(items[1].id, "race-r070");
    }

    #[test]
    fn parses_ics_and_drops_ended_events() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:future-ev@bay-ai-calendar\r\nSUMMARY:未来大会\r\nDTSTART;VALUE=DATE:29991016\r\nDTEND;VALUE=DATE:29991018\r\nLOCATION:深圳·南山\r\nURL:https://example.com/ev\r\nCATEGORIES:深圳\\,技术大会\r\nDESCRIPTION:简介第一行\\\n第二行被折\r\n 行到这里\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:past-ev@bay-ai-calendar\r\nSUMMARY:去年的会\r\nDTSTART;VALUE=DATE:20200101\r\nDTEND;VALUE=DATE:20200103\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let items = parse_ics(ics).unwrap();
        assert_eq!(items.len(), 1, "已结束的被过滤");
        assert_eq!(items[0].id, "wqai-future-ev");
        assert_eq!(items[0].title, "未来大会");
        assert_eq!(items[0].link, "https://example.com/ev");
        assert_eq!(items[0].published, "2999-10-16");
        assert!(items[0].summary.contains("深圳 · 技术大会"));
        assert!(items[0].summary.contains("深圳·南山"));
        assert!(items[0].summary.contains("2999-10-16 ~ 2999-10-18"));
    }

    #[test]
    fn ics_timestamps_handle_z_and_local() {
        assert_eq!(ics_ts("20261016"), ics_ts("20261016T000000"));
        // 带 Z 的 UTC 时间比东八区口径早 8 小时
        assert_eq!(ics_ts("20261016T080000Z"), ics_ts("20261016T160000"),);
        assert_eq!(ics_date_str("20261016T133000"), Some("2026-10-16".into()));
    }
}
