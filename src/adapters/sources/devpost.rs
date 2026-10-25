//! Devpost 黑客松平台：官方搜索 API（/api/hackathons）返回结构化 JSON，
//! 在 rss 模块里以 `devpost+<api-url>` 前缀接入，与 RSS 条目同构进候选池。

use super::feed::{FeedItem, fetch_text, strip_tags};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：
/// `devpost+https://devpost.com/api/hackathons?status%5B%5D=upcoming&status%5B%5D=open`
pub const URL_PREFIX: &str = "devpost+";

/// 注册表条目：`devpost+<api-url>` 路由到本源。
pub const DEF: super::SourceDef = super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { DevpostClient::new().fetch_hackathons(&url).await }),
};

pub struct DevpostClient {
    http: reqwest::Client,
}

impl Default for DevpostClient {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, serde::Deserialize)]
struct DevpostHackathon {
    id: i64,
    title: String,
    url: String,
    open_state: Option<String>,
    submission_period_dates: Option<String>,
    time_left_to_submission: Option<String>,
    prize_amount: Option<String>,
    registrations_count: Option<i64>,
    organization_name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct DevpostResponse {
    hackathons: Vec<DevpostHackathon>,
}

impl DevpostClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 调搜索 API 拉赛事列表；只保留 open/upcoming 状态的条目。
    pub async fn fetch_hackathons(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(
            self.http
                .get(url)
                .header("Accept", "application/json")
                .header("X-Requested-With", "XMLHttpRequest"),
            "devpost",
        )
        .await?;
        let response: DevpostResponse =
            serde_json::from_str(&body).context("devpost json parse failed")?;
        Ok(response
            .hackathons
            .into_iter()
            .filter(|h| matches!(h.open_state.as_deref(), Some("open") | Some("upcoming")))
            .map(|h| {
                let prize = h
                    .prize_amount
                    .map(|p| strip_tags(&p))
                    .filter(|p| !p.is_empty() && *p != "0")
                    .unwrap_or_else(|| "未标注".into());
                let mut summary_parts = vec![];
                if let Some(org) = &h.organization_name {
                    summary_parts.push(org.clone());
                }
                if let Some(period) = &h.submission_period_dates {
                    summary_parts.push(format!("提交窗口 {period}"));
                }
                if let Some(left) = &h.time_left_to_submission {
                    if !left.is_empty() {
                        summary_parts.push(left.clone());
                    }
                }
                summary_parts.push(format!("奖金 {prize}"));
                if let Some(count) = h.registrations_count {
                    summary_parts.push(format!("报名 {count}"));
                }
                FeedItem {
                    id: format!("devpost-{}", h.id),
                    title: h.title,
                    link: h.url,
                    summary: summary_parts.join(" · "),
                    published: String::new(),
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_response_and_filters_closed() {
        let body = r#"{"hackathons":[
          {"id":31389,"title":"OxHack 2026","url":"https://oxhack-2026.devpost.com/",
           "open_state":"open","submission_period_dates":"Oct 24 - 25, 2026",
           "time_left_to_submission":"about 2 hours left",
           "prize_amount":"£<span data-currency-value>1000</span>",
           "registrations_count":88,"organization_name":"Oxford University"},
          {"id":1,"title":"已结束","url":"https://x.devpost.com/","open_state":"finished",
           "submission_period_dates":null,"time_left_to_submission":null,
           "prize_amount":null,"registrations_count":null,"organization_name":null}
        ]}"#;
        let response: DevpostResponse = serde_json::from_str(body).unwrap();
        let items: Vec<FeedItem> = response
            .hackathons
            .into_iter()
            .filter(|h| matches!(h.open_state.as_deref(), Some("open") | Some("upcoming")))
            .map(|h| FeedItem {
                id: format!("devpost-{}", h.id),
                title: h.title,
                link: h.url,
                summary: String::new(),
                published: String::new(),
            })
            .collect();
        assert_eq!(items.len(), 1, "finished 被过滤");
        assert_eq!(items[0].id, "devpost-31389");
        assert_eq!(strip_tags("£<span data-currency-value>1000</span>"), "£1000");
    }
}
