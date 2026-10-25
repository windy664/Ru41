//! lablab.ai：纯 AI 应用黑客松平台。列表页服务端渲染，
//! 赛事清单在 ld+json 的 ItemList 里（name + url），无需逆向 API。
//! 在 rss 模块里以 `lablab+<page-url>` 前缀接入。
//! 列表无日期字段，靠 slug 去重：只报"新出现"的赛事，符合雷达口径。

use super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`lablab+https://lablab.ai/ai-hackathons`。
pub const URL_PREFIX: &str = "lablab+";

/// 注册表条目：`lablab+<page-url>` 路由到本源。
pub const DEF: super::SourceDef = super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { LablabClient::new().fetch_hackathons(&url).await }),
};

/// 列表共 100+ 条（含进行中与即将开始），只取最前面的一屏，
/// 避免首次接入时往候选池灌入整站历史。
const MAX_ITEMS: usize = 30;

pub struct LablabClient {
    http: reqwest::Client,
}

impl Default for LablabClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LablabClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_hackathons(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(self.http.get(url), "lablab").await?;
        parse_item_list(&body)
    }
}

#[derive(Debug, serde::Deserialize)]
struct ItemListElement {
    name: String,
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct ItemList {
    #[serde(rename = "itemListElement")]
    item_list_element: Vec<ItemListElement>,
}

/// 从 ld+json 的 @graph 里找 ItemList。页面有多个 ld+json 块，
/// 逐个尝试解析，找到含 ItemList 的为止。
fn parse_item_list(html: &str) -> Result<Vec<FeedItem>> {
    let mut rest = html;
    while let Some(start) = rest.find("<script type=\"application/ld+json\">") {
        let json_start = start + "<script type=\"application/ld+json\">".len();
        let Some(end) = rest[json_start..].find("</script>") else {
            break;
        };
        let raw = &rest[json_start..json_start + end];
        rest = &rest[json_start + end..];
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(raw) else {
            continue;
        };
        let Some(graph) = doc.get("@graph").and_then(|g| g.as_array()) else {
            continue;
        };
        for node in graph {
            if node.get("@type").and_then(|t| t.as_str()) != Some("ItemList") {
                continue;
            }
            let list: ItemList =
                serde_json::from_value(node.clone()).context("lablab ItemList parse failed")?;
            return Ok(list
                .item_list_element
                .into_iter()
                .take(MAX_ITEMS)
                .map(|e| {
                    let slug = e.url.rsplit('/').next().unwrap_or(&e.url).to_owned();
                    FeedItem {
                        id: format!("lablab-{slug}"),
                        title: e.name,
                        link: e.url,
                        summary: "lablab.ai AI 黑客松".into(),
                        published: String::new(),
                    }
                })
                .collect());
        }
    }
    anyhow::bail!("lablab page has no ld+json ItemList")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_item_list_from_graph() {
        let html = r#"<html><head>
<script type="application/ld+json">{"@context":"https://schema.org","@graph":[
{"@type":"CollectionPage","name":"AI Hackathons"},
{"@type":"ItemList","itemListElement":[
 {"@type":"ListItem","position":1,"name":"Vultr: Agent Rush Hackathon","url":"https://lablab.ai/ai-hackathons/vultr-hackathon"},
 {"@type":"ListItem","position":2,"name":"AMD AI League","url":"https://lablab.ai/ai-hackathons/amd-lablab-ai-academy-challenge"}
]}
]}</script></head><body></body></html>"#;
        let items = parse_item_list(html).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, "lablab-vultr-hackathon");
        assert_eq!(items[0].title, "Vultr: Agent Rush Hackathon");
        assert_eq!(
            items[1].link,
            "https://lablab.ai/ai-hackathons/amd-lablab-ai-academy-challenge"
        );
    }

    #[test]
    fn missing_item_list_is_error() {
        assert!(parse_item_list("<html><body>no data</body></html>").is_err());
    }
}
