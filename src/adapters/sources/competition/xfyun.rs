//! 讯飞 AI 开发者大赛（challenge.xfyun.cn）：Next.js 静态导出站，数据全靠
//! 客户端渲染，无公开 API——经 ru41-browser（browserless /content）渲染后
//! 解析赛事卡片。在 rss 模块里以 `xfyun+<page-url>` 前缀接入。
//! 该站赛道多、奖金明确、含智能体应用赛，是重点在盯的国内源。

use super::super::feed::FeedItem;
use anyhow::{Context, Result};
use std::time::Duration;

/// browser 渲染允许的整轮等待上限（含页面 networkidle2）。
const RENDER_TIMEOUT: Duration = Duration::from_secs(75);

/// rss feed 列表里的接入前缀：`xfyun+https://challenge.xfyun.cn/competition`。
pub const URL_PREFIX: &str = "xfyun+";

/// 注册表条目：`xfyun+<page-url>` 路由到本源（走浏览器渲染）。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { XfyunClient::new().fetch_competitions(&url).await }),
};

pub struct XfyunClient {
    http: reqwest::Client,
    /// browserless /content 端点；默认取 ru41-net 里的 ru41-browser 容器。
    browser_url: String,
}

impl Default for XfyunClient {
    fn default() -> Self {
        Self::new()
    }
}

impl XfyunClient {
    pub fn new() -> Self {
        // ru41-browser 是 ru41-net 内的容器地址，不能走容器注入的出站代理
        // （HTTP_PROXY 指向 mihomo，代理解析不了容器名，会回 502）——单独建
        // 一个 no_proxy 客户端。
        Self {
            http: reqwest::Client::builder()
                .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
                .timeout(RENDER_TIMEOUT)
                .no_proxy()
                .build()
                .expect("xfyun render client build"),
            browser_url: std::env::var("RU41_BROWSER_URL")
                .unwrap_or_else(|_| "http://ru41-browser:3000".into()),
        }
    }

    /// 渲染列表页并解析赛事卡片；只保留进行中/未开始的（答辩结束/已完赛丢弃）。
    pub async fn fetch_competitions(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = self
            .http
            .post(format!("{}/content", self.browser_url))
            .header("Content-Type", "application/json")
            .body(format!(
                r#"{{"url":"{url}","gotoOptions":{{"waitUntil":"networkidle2","timeout":30000}}}}"#
            ))
            .send()
            .await
            .context("xfyun render request failed")?
            .error_for_status()
            .context("xfyun render returned error status")?
            .text()
            .await
            .context("xfyun render body read failed")?;
        Ok(parse_competition_page(&body, url))
    }
}

/// 解析列表页卡片：competition_card_list_item 块，逐块提取
/// 标题/举办方/奖金/分类/报名截止/状态。已结束的条目丢弃。
fn parse_competition_page(html: &str, page_url: &str) -> Vec<FeedItem> {
    html.split("<div class=\"competition_card_list_item\"")
        .skip(1)
        .filter_map(|block| parse_card(block, page_url))
        .collect()
}

fn parse_card(block: &str, page_url: &str) -> Option<FeedItem> {
    let title = capture(block, r#"card_header_title">([^<]+)</div>"#)?;
    let sponsor = capture(block, r#"match_holders">举办方[：:]\s*([^<]+)</div>"#);
    let prize = capture(
        block,
        r#"card_right_count_item_value"[^>]*>([￥¥][^<]+)</div>"#,
    );
    let category = capture(block, r#"card-industry-tag"[^>]*>(#[^<]+)</div>"#);
    let deadline = capture(block, r#"card_right_time_title">报名截至[：:]\s*([0-9-]+)"#);
    let status = capture(block, r#"card_right_time_status">.*?<span>([^<]+)</span>"#);
    // 已完赛/答辩结束的条目对雷达无意义
    if let Some(s) = &status
        && (s.contains("结束") || s.contains("完赛"))
    {
        return None;
    }
    let mut summary_parts = vec![];
    if let Some(v) = sponsor {
        summary_parts.push(format!("举办方 {v}"));
    }
    if let Some(v) = prize {
        summary_parts.push(format!("奖金 {v}"));
    }
    if let Some(v) = &category {
        summary_parts.push(v.clone());
    }
    if let Some(v) = &deadline {
        summary_parts.push(format!("报名截止 {v}"));
    }
    if let Some(v) = status {
        summary_parts.push(format!("状态 {v}"));
    }
    // 无公开详情 id，用标题+截止做去重键
    let id_key = format!("{}:{}", title, deadline.as_deref().unwrap_or(""));
    Some(FeedItem {
        id: format!("xfyun-{:x}", fnv1a(&id_key)),
        title: title.clone(),
        link: page_url.to_owned(),
        summary: summary_parts.join(" · "),
        published: deadline.unwrap_or_default(),
    })
}

fn capture(text: &str, pattern: &str) -> Option<String> {
    let re = regex::Regex::new(pattern).ok()?;
    let caps = re.captures(text)?;
    let value = caps.get(1)?.as_str().trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// 轻量稳定散列（无需引入新依赖）。
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in text.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
<div class="competition_card_list_item"><div class="competition_card_top">
<div class="card_header"><div class="card_header_title">Spark-X2.5端侧模型创新挑战赛</div></div>
<div class="card_sponsorName"><div class="match_holders">举办方：词元星火科技有限公司</div></div>
<div class="card_right_count_item_value" style="color: rgb(25, 117, 255);">￥100000</div>
<div class="card-tag-list"><div class="card-industry-tag">#大模型技术</div></div>
<div class="card_right_time"><div class="card_right_time_title">报名截至： 2026-11-10</div>
<div class="card_right_time_status"><div class="playing"><span>火热报名中</span></div></div></div></div>
<div class="competition_card_list_item"><div class="competition_card_top">
<div class="card_header"><div class="card_header_title">语音质量修复挑战赛 赛季2</div></div>
<div class="card_sponsorName"><div class="match_holders">举办方：声网</div></div>
<div class="card_right_count_item_value">￥10000</div>
<div class="card_right_time"><div class="card_right_time_title">报名截至： 2026-09-24</div>
<div class="card_right_time_status"><div class="ended"><span>答辩结束</span></div></div></div></div>
"#;

    #[test]
    fn parses_cards_and_drops_ended() {
        let items = parse_competition_page(FIXTURE, "https://challenge.xfyun.cn/competition");
        assert_eq!(items.len(), 1, "答辩结束的条目被过滤");
        let item = &items[0];
        assert_eq!(item.title, "Spark-X2.5端侧模型创新挑战赛");
        assert!(item.summary.contains("举办方 词元星火科技有限公司"));
        assert!(item.summary.contains("奖金 ￥100000"));
        assert!(item.summary.contains("#大模型技术"));
        assert!(item.summary.contains("报名截止 2026-11-10"));
        assert!(item.summary.contains("状态 火热报名中"));
        assert_eq!(item.published, "2026-11-10");
        assert!(item.id.starts_with("xfyun-"));
    }

    #[test]
    fn id_is_stable_per_title_and_deadline() {
        assert_eq!(fnv1a("a:2026-11-10"), fnv1a("a:2026-11-10"));
        assert_ne!(fnv1a("a:2026-11-10"), fnv1a("a:2026-11-11"));
    }
}
