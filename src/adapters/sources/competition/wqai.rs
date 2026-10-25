//! 湾区 AI 日历（wqai.ltd）：粤港澳大湾区活动与赛事聚合站。
//! 两类接入：赛事页（calendar/race/）解析静态表格；官方公共 ICS 订阅源
//! （calendar/calendar.ics， llms.txt 公示、无需订阅码）解析 VEVENT，
//! 覆盖活动+赛事，UID 天然去重，含城市/场馆/票务等富描述。
//! 在 rss 模块里以 `wqai+<url>` 前缀接入，产出与 RSS 条目同构的 FeedItem 进候选池。

use super::super::feed::{FeedItem, fetch_text, strip_tags};
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

/// 解析赛事页表格：<tr><td>截止</td><td><a href="race-rNNN.html">名称</a></td>
/// <td>地点</td><td>组队</td><td>奖励</td><td>状态</td></tr>。
/// 只取带 race- 链接的条目；已关闭区（Closed）的条目状态文本不同但也带链接，
/// 靠 id 去重后重开/变更不会再入池，符合"只报新出现"的口径。
fn parse_race_page(html: &str, page_url: &str) -> Result<Vec<FeedItem>> {
    let base = page_url.rsplit_once('/').map(|(b, _)| b).unwrap_or("");
    let mut items = Vec::new();
    let mut rest = html;
    while let Some(row_start) = rest.find("<tr><td>") {
        // 只跳过 "<tr>"，保留首个 "<td>" 让 split("<td>") 的列对齐
        rest = &rest[row_start + 4..];
        let row_end = rest.find("</tr>").unwrap_or(rest.len());
        let row = &rest[..row_end];
        rest = &rest[row_end..];
        let Some(item) = parse_row(row, base) else {
            continue;
        };
        items.push(item);
    }
    Ok(items)
}

fn parse_row(row: &str, base: &str) -> Option<FeedItem> {
    let cells: Vec<&str> = row.split("<td>").skip(1).collect();
    if cells.len() < 6 {
        return None;
    }
    let cell_text = |i: usize| -> String {
        let raw = cells[i].split("</td>").next().unwrap_or("");
        strip_tags(raw)
    };
    // 名称列必须是 race- 详情链接，否则不是赛事条目
    let link_cell = cells[1];
    let href_start = link_cell.find("href=\"")? + 6;
    let href_end = link_cell[href_start..].find('"')? + href_start;
    let href = &link_cell[href_start..href_end];
    if !href.starts_with("race-") {
        return None;
    }
    let title = cell_text(1);
    if title.is_empty() {
        return None;
    }
    // 只要开放中的条目：关闭/截止的带 st closed 等其他 class，直接跳过
    if !row.contains("st open") {
        return None;
    }
    let deadline = cell_text(0);
    let venue = cell_text(2);
    let team = cell_text(3);
    let rewards = cell_text(4);
    let status = cell_text(5);
    Some(FeedItem {
        id: href.trim_end_matches(".html").to_owned(),
        title: title.clone(),
        link: format!("{base}/{href}"),
        summary: format!("截止 {deadline} · {venue} · {team} · 奖励：{rewards} · 状态：{status}"),
        published: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
<table class="tbl"><tr><th>Deadline</th><th>Competitions</th></tr>
<tr><td>10-28</td><td><a href="race-r009.html">2026 National AI+ Competition</a></td><td>Online</td><td>Individual or team</td><td>E-certificates</td><td><span class="st open">Closing in 3 days</span></td></tr>
<tr><td>10-30</td><td><a href="race-r070.html">ClawHunt 72-Hour Hackathon</a></td><td>Shanghai</td><td>3-5</td><td>RMB 1.5M</td><td><span class="st open">Open</span></td></tr>
<tr><td>--</td><td>无链接行</td><td>-</td><td>-</td><td>-</td><td>-</td></tr>
<tr><td>10-01</td><td><a href="race-r001.html">已结束的比赛</a></td><td>-</td><td>-</td><td>-</td><td><span class="st closed">Closed</span></td></tr>
</table>"#;

    #[test]
    fn parses_race_rows_with_stable_ids() {
        let items = parse_race_page(FIXTURE, "https://wqai.ltd/calendar/race/index.html").unwrap();
        assert_eq!(items.len(), 2, "无链接行和已关闭条目被跳过");
        assert_eq!(items[0].id, "race-r009");
        assert_eq!(items[0].title, "2026 National AI+ Competition");
        assert_eq!(
            items[0].link,
            "https://wqai.ltd/calendar/race/race-r009.html"
        );
        assert!(items[0].summary.contains("截止 10-28"));
        assert!(items[0].summary.contains("Closing in 3 days"));
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
