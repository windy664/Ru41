//! 湾区 AI 日历（wqai.ltd）赛事页解析：静态表格结构稳定，条目 id（race-rNNN）天然去重。
//! 在 rss 模块里以 `wqai+<url>` 前缀接入，产出与 RSS 条目同构的 FeedItem 进候选池。

use super::feed::FeedItem;
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`wqai+https://wqai.ltd/calendar/race/index.html`。
pub const URL_PREFIX: &str = "wqai+";

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

    /// 抓取赛事页并解析出「开放中」条目的 FeedItem 列表。
    pub async fn fetch_race_items(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = self
            .http
            .get(url)
            .send()
            .await
            .context("wqai request failed")?
            .error_for_status()
            .context("wqai page returned error status")?
            .text()
            .await
            .context("wqai body read failed")?;
        parse_race_page(&body, url)
    }
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

fn strip_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    // HTML 实体只处理常见的几个（&quot; &amp; 等）
    out.replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&nbsp;", " ")
        .trim()
        .to_owned()
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
        assert_eq!(items[0].link, "https://wqai.ltd/calendar/race/race-r009.html");
        assert!(items[0].summary.contains("截止 10-28"));
        assert!(items[0].summary.contains("Closing in 3 days"));
        assert_eq!(items[1].id, "race-r070");
    }
}
