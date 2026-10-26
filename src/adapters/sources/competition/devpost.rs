//! Devpost 黑客松平台：官方搜索 API（/api/hackathons）返回结构化 JSON，
//! 在 rss 模块里以 `devpost+<api-url>` 前缀接入，与 RSS 条目同构进候选池。

use super::super::feed::{FeedItem, fetch_text, strip_tags};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：
/// `devpost+https://devpost.com/api/hackathons?status%5B%5D=upcoming&status%5B%5D=open`
pub const URL_PREFIX: &str = "devpost+";

/// 注册表条目：`devpost+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
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
pub struct DevpostHackathon {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub open_state: Option<String>,
    pub submission_period_dates: Option<String>,
    pub time_left_to_submission: Option<String>,
    pub prize_amount: Option<String>,
    pub registrations_count: Option<i64>,
    pub organization_name: Option<String>,
    #[serde(default)]
    pub themes: Vec<DevpostTheme>,
    #[serde(default)]
    pub displayed_location: Option<DevpostLocation>,
}

#[derive(Debug, serde::Deserialize)]
pub struct DevpostTheme {
    pub name: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct DevpostLocation {
    pub location: String,
}

impl DevpostHackathon {
    /// 奖金文本（剥 HTML 标签）。
    pub fn prize_text(&self) -> String {
        self.prize_amount
            .as_deref()
            .map(strip_tags)
            .unwrap_or_default()
    }

    /// 是否纯线上。
    pub fn is_online(&self) -> bool {
        self.displayed_location
            .as_ref()
            .is_some_and(|l| l.location.eq_ignore_ascii_case("online"))
    }

    pub fn theme_names(&self) -> Vec<String> {
        self.themes.iter().map(|t| t.name.clone()).collect()
    }
}

/// 解析提交窗口的结束日："Oct 23 - 25, 2026" / "Oct 23 - Nov 25, 2026"。
pub fn parse_submission_end(period: &str) -> Option<chrono::NaiveDate> {
    let re = regex::Regex::new(r"(\w+) (\d+) - (?:(\w+) )?(\d+), (\d{4})").ok()?;
    let caps = re.captures(period)?;
    let month = caps
        .get(3)
        .map_or_else(|| caps[1].to_owned(), |m| m.as_str().to_owned());
    chrono::NaiveDate::parse_from_str(&format!("{} {} {}", month, &caps[4], &caps[5]), "%b %d %Y")
        .ok()
}

#[derive(Debug, serde::Deserialize)]
struct DevpostMeta {
    total_count: Option<usize>,
}

#[derive(Debug, serde::Deserialize)]
struct DevpostResponse {
    hackathons: Vec<DevpostHackathon>,
    #[serde(default)]
    meta: Option<DevpostMeta>,
}

impl DevpostClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 调搜索 API 拉赛事列表（类型化条目，含 open/upcoming 过滤前的全量）。
    /// API 默认每页只有 9 条，per_page 上限 40（2026-10 实测），而在办赛事
    /// 有 160+ 场——不分页会漏掉截止日靠后的（PayPal AI Hackathon 曾因此漏捕）。
    /// 最多 6 页，或累计达到 total_count / 页面无新增时提前结束。
    pub async fn fetch_all(&self, url: &str) -> Result<Vec<DevpostHackathon>> {
        const MAX_PAGES: u32 = 6;
        const PER_PAGE: u32 = 40;
        let sep = if url.contains('?') { '&' } else { '?' };
        let mut all: Vec<DevpostHackathon> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        for page in 1..=MAX_PAGES {
            let paged = format!("{url}{sep}per_page={PER_PAGE}&page={page}");
            let body = fetch_text(
                self.http
                    .get(&paged)
                    .header("Accept", "application/json")
                    .header("X-Requested-With", "XMLHttpRequest"),
                "devpost",
            )
            .await?;
            let response: DevpostResponse =
                serde_json::from_str(&body).context("devpost json parse failed")?;
            let total = response.meta.and_then(|m| m.total_count);
            let mut new_on_page = 0;
            for h in response.hackathons {
                if seen_ids.insert(h.id) {
                    all.push(h);
                    new_on_page += 1;
                }
            }
            // 空页/无新增（页序漂移）/已拿全：停
            if new_on_page == 0 || total.is_some_and(|t| all.len() >= t) {
                break;
            }
        }
        Ok(all)
    }

    /// 雷达入口：fetch_all 之上只保留 open/upcoming 并映射成 FeedItem。
    pub async fn fetch_hackathons(&self, url: &str) -> Result<Vec<FeedItem>> {
        Ok(self
            .fetch_all(url)
            .await?
            .into_iter()
            .filter(|h| matches!(h.open_state.as_deref(), Some("open") | Some("upcoming")))
            .map(|h| {
                // $0 是"无奖金"不是"未知"：如实标注，防 digest 模型脑补金额。
                let prize = h
                    .prize_amount
                    .map(|p| strip_tags(&p))
                    .filter(|p| !p.is_empty())
                    .map(|p| if p == "0" { "无奖金".into() } else { p })
                    .unwrap_or_else(|| "未标注".into());
                let mut summary_parts = vec![];
                if let Some(org) = &h.organization_name {
                    summary_parts.push(org.clone());
                }
                if let Some(period) = &h.submission_period_dates {
                    summary_parts.push(format!("提交窗口 {period}"));
                }
                if let Some(left) = &h.time_left_to_submission
                    && !left.is_empty()
                {
                    summary_parts.push(left.clone());
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
        assert_eq!(
            strip_tags("£<span data-currency-value>1000</span>"),
            "£1000"
        );
    }
}
