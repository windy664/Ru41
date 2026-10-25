//! DataFountain（datafountain.cn）：国内数据科学竞赛平台（CCF 系）。
//! 列表页服务端渲染，每个赛事是 `<div class="compt-item ...">` 块，
//! 内含标题/链接/主办方/赛期/奖金/组队人数。在 rss 模块里以
//! `datafountain+<page-url>` 前缀接入。只收当前可报名（can-join）的条目。

use super::feed::FeedItem;
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`datafountain+https://www.datafountain.cn/competitions`。
pub const URL_PREFIX: &str = "datafountain+";

pub struct DataFountainClient {
    http: reqwest::Client,
}

impl Default for DataFountainClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DataFountainClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_competitions(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = self
            .http
            .get(url)
            .send()
            .await
            .context("datafountain request failed")?
            .error_for_status()
            .context("datafountain page returned error status")?
            .text()
            .await
            .context("datafountain body read failed")?;
        Ok(parse_competition_page(&body))
    }
}

/// 解析列表页：按 compt-item 块切分，逐块提取字段。
/// 只保留可报名的（state-join can-join）；已结束的条目没有该 class。
fn parse_competition_page(html: &str) -> Vec<FeedItem> {
    html.split("<div class=\"compt-item ")
        .skip(1)
        .filter_map(parse_item_block)
        .collect()
}

fn parse_item_block(block: &str) -> Option<FeedItem> {
    if !block.contains("state-join can-join") {
        return None;
    }
    let link_path = capture(block, r#"href="(/competitions/\d+)"#)?;
    let id_num = link_path.rsplit('/').next()?;
    let title = capture(block, r#"compt__title[^"]*"[^>]*>\s*(.*?)\s*</a>"#)?;
    let sponsor = capture(block, r#"organizer[：:]</span>\s*<span>\s*([^<]+?)\s*</span>"#);
    let dates = capture(
        block,
        r#"competition date[：:]</span>\s*<span>([^<]+)</span>\s*<span>-</span>\s*<span>([^<]+)</span>"#,
    );
    let prize = capture(block, r#"oview__content"><span>([^<]+)</span>"#);
    let mut summary_parts = vec![];
    if let Some(s) = sponsor {
        summary_parts.push(format!("主办 {s}"));
    }
    if let Some(d) = dates {
        summary_parts.push(format!("赛期 {d}"));
    }
    if let Some(p) = prize {
        summary_parts.push(format!("奖金 {p}"));
    }
    Some(FeedItem {
        id: format!("df-{id_num}"),
        title,
        link: format!("https://www.datafountain.cn{link_path}"),
        summary: summary_parts.join(" · "),
        published: String::new(),
    })
}

/// 小组捕获：单分组返回该组，多分组用 " - " 连接成一段。
fn capture(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(text)?;
    let parts: Vec<&str> = (1..=caps.len().saturating_sub(1))
        .filter_map(|i| caps.get(i).map(|m| m.as_str().trim()))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" - "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
<div class="compt-item block-link out_service"><div class="state-join can-join">allowed</div>
<div class="compt-item-top"><a href="/competitions/1169" class="compt__pic"><img/></a>
<div class="compt-info"><div class="compt__header"><span class="compt__cmpt-type">Grand prix</span>
<a href="/competitions/1169" class="compt__title"> AI新睿人才“星探计划” </a></div>
<div class="compt__sponsor"><div class="compt__sponsor-name"><span><i></i> organizer：</span>
<span> 烟台睿创微纳技术股份有限公司 </span></div></div>
<div class="cmp-date"><span><i></i> competition date：</span> <span>2026.04.07</span> <span>-</span> <span>2026.08.31</span></div>
<div class="oview__content"><span>￥90,000</span></div></div></div></div>
<div class="compt-item block-link"><div class="state-join">ended</div>
<a href="/competitions/1000" class="compt__title"> 已结束的比赛 </a></div>
"#;

    #[test]
    fn parses_joinable_items_only() {
        let items = parse_competition_page(FIXTURE);
        assert_eq!(items.len(), 1, "已结束的条目被过滤");
        assert_eq!(items[0].id, "df-1169");
        assert_eq!(items[0].title, "AI新睿人才“星探计划”");
        assert_eq!(
            items[0].link,
            "https://www.datafountain.cn/competitions/1169"
        );
        assert!(items[0].summary.contains("主办 烟台睿创微纳"));
        assert!(items[0].summary.contains("赛期 2026.04.07 - 2026.08.31"));
        assert!(items[0].summary.contains("奖金 ￥90,000"));
    }
}
