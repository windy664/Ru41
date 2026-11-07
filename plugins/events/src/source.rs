//! 茶思屋（chaspark，华为旗下学术平台）会议/活动日历公开接口。
//! 逆向自 `#/stw/activities` 页面 XHR（2026-11-01，免登录）：
//! `GET /chasiwu/v1/content/conferences/calendar/list?startTime=<unix>&endTime=<unix>&lang=zh`
//! 返回 code=0 + data 数组，会议/论坛/直播混合（链接多为 #/live/<id>）。
//! 注意区别：`/content/calendar/list` 是直播日历（恒 20 条不分页），
//! 会议日历是 `conferences/calendar/list`（官方 API 里 confereces 是站方拼写，别改）。

use anyhow::{Context, Result, bail};
use chrono::TimeZone;

pub const CALENDAR_PATH: &str = "/chasiwu/v1/content/conferences/calendar/list";
pub const DEFAULT_BASE: &str = "https://www.chaspark.com";

/// 单条活动（会议/论坛/直播讲座统一）。
#[derive(Clone, Debug)]
pub struct EventItem {
    pub id: String,
    pub title: String,
    pub start: i64,
    pub end: Option<i64>,
    pub location: Option<String>,
    pub url: String,
    pub domains: Vec<String>,
}

#[derive(Clone)]
pub struct ChasparkClient {
    http: reqwest::Client,
    base: String,
}

impl ChasparkClient {
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    pub fn with_base(base: &str) -> Self {
        let http = ru41::util::http_client_builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) ru41-events")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("events http client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    /// 拉取 [start, end] 时间窗内的活动（unix 秒）。
    pub async fn fetch_calendar(&self, start: i64, end: i64) -> Result<Vec<EventItem>> {
        let value: serde_json::Value = self
            .http
            .get(format!("{}{CALENDAR_PATH}", self.base))
            .query(&[
                ("startTime", start.to_string()),
                ("endTime", end.to_string()),
                ("lang", "zh".to_owned()),
            ])
            .send()
            .await
            .context("茶思屋会议日历请求失败")?
            .json()
            .await
            .context("茶思屋会议日历响应不是 JSON")?;
        // code 时而数字 0 时而字符串 "0"（2026-11-01 实测两种都出现过），都认。
        let code = value.get("code");
        let ok = matches!(code, Some(serde_json::Value::Number(n)) if n.as_i64() == Some(0))
            || matches!(code, Some(serde_json::Value::String(s)) if s == "0");
        if !ok {
            bail!(
                "茶思屋会议日历返回异常：{}",
                value
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            );
        }
        let list = value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(list.iter().filter_map(parse_event).collect())
    }
}

fn parse_event(value: &serde_json::Value) -> Option<EventItem> {
    // 注意：会议日历的条目没有 id 字段，只有 contentId（直播日历才有 id）。
    let id = value
        .get("id")
        .and_then(as_text)
        .or_else(|| value.get("contentId").and_then(as_text))?;
    let title = value
        .get("titleZh")
        .and_then(as_text)
        .or_else(|| value.get("titleEn").and_then(as_text))
        .filter(|t| !t.is_empty())?;
    let start = value.get("startTime").and_then(as_i64)?;
    let end = value.get("endTime").and_then(as_i64);
    let location = value
        .get("location")
        .and_then(as_text)
        .filter(|s| !s.is_empty());
    let url = value
        .get("links")
        .and_then(as_text)
        .unwrap_or_else(|| DEFAULT_BASE.to_owned());
    let domains = value
        .get("domains")
        .and_then(serde_json::Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|d| d.get("domainName").and_then(as_text))
                .collect()
        })
        .unwrap_or_default();
    Some(EventItem {
        id,
        title,
        start,
        end,
        location,
        url,
        domains,
    })
}

/// 站方字段时而是字符串时而是数字，两种都收。
fn as_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

// ---------- 活动行（huodongxing）：民间活动最大平台 ----------

/// 活动行城市列表页是 SSR（RSSHub 路由也是这么抓的），字段结构稳定：
/// `a.item-title` 标题+链接、`p.item-data` 日期「2026.11.10-2026.11.12」、
/// `p.item-dress` 地点「广东广州广交会展馆C区」。
pub struct HuodongxingClient {
    http: reqwest::Client,
}

/// 盯防城市：大湾区核心三城。
pub const HDX_CITIES: &[&str] = &["广州", "深圳", "香港"];

impl HuodongxingClient {
    pub fn new() -> Self {
        let http = ru41::util::http_client_builder()
            .user_agent("Mozilla/5.0 (X11; Linux x86_64) ru41-events")
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("hdx http client");
        Self { http }
    }

    /// 抓一个城市列表页（一页 ~13 条，无需翻页——只要最近的）。
    pub async fn fetch_city(&self, city: &str) -> Result<Vec<EventItem>> {
        let url = format!("https://www.huodongxing.com/eventlist?city={city}");
        let html = self
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("活动行城市页请求失败: {city}"))?
            .text()
            .await
            .context("活动行城市页读取失败")?;
        Ok(parse_hdx_list(&html))
    }
}

fn parse_hdx_list(html: &str) -> Vec<EventItem> {
    let block_re =
        regex::Regex::new(r#"(?s)<a class="item-title" href="(/event/\d+)[^"]*"[^>]*>([^<]+)</a>.*?item-data[^>]*>.*?</span>([^<]+)</p>.*?item-dress[^>]*>.*?</span>([^<]+)</p>"#)
            .expect("hdx item regex");
    block_re
        .captures_iter(html)
        .filter_map(|cap| {
            let id = cap[1].trim_start_matches("/event/").to_owned();
            let title = cap[2].trim().to_owned();
            let (start, end) = parse_hdx_dates(cap[3].trim());
            let start = start?;
            Some(EventItem {
                url: format!("https://www.huodongxing.com{}", &cap[1]),
                id: format!("hdx-{id}"),
                title,
                start,
                end,
                location: Some(cap[4].trim().to_owned()).filter(|s| !s.is_empty()),
                domains: Vec::new(),
            })
        })
        .collect()
}

/// 「2026.11.10-2026.11.12」→（开始， 结束）；单天「2026.11.10」也行。
fn parse_hdx_dates(text: &str) -> (Option<i64>, Option<i64>) {
    let day_re = regex::Regex::new(r"(\d{4})[.\-/](\d{1,2})[.\-/](\d{1,2})").expect("date regex");
    let mut days = day_re
        .captures_iter(text)
        .filter_map(|cap| {
            let (y, m, d) = (
                cap[1].parse().ok()?,
                cap[2].parse().ok()?,
                cap[3].parse().ok()?,
            );
            chrono::NaiveDate::from_ymd_opt(y, m, d)
        });
    let first = days.next();
    let second = days.next();
    let to_ts = |day: chrono::NaiveDate, hour: u32| {
        day.and_hms_opt(hour, 0, 0)
            .and_then(|dt| chrono::Local.from_local_datetime(&dt).single())
            .map(|t| t.timestamp())
    };
    (
        first.and_then(|d| to_ts(d, 9)),
        second.or(first).and_then(|d| to_ts(d, 18)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_event_with_string_timestamps() {
        let value = serde_json::json!({
            "id": "1333595930114838528",
            "titleZh": "CCF 决策智能会议（RLChina 2026）",
            "startTime": "1793689200",
            "endTime": "1793692800",
            "location": null,
            "links": "https://www.chaspark.com/#/live/1333595930114838528",
            "domains": [{"domainName": "人工智能领域"}]
        });
        let event = parse_event(&value).expect("event");
        assert_eq!(event.title, "CCF 决策智能会议（RLChina 2026）");
        assert_eq!(event.start, 1793689200);
        assert_eq!(event.end, Some(1793692800));
        assert_eq!(event.location, None);
        assert_eq!(event.domains, vec!["人工智能领域"]);
    }

    #[test]
    fn skips_event_without_title_or_start() {
        assert!(parse_event(&serde_json::json!({"id": "1"})).is_none());
        assert!(parse_event(&serde_json::json!({"id": "1", "titleZh": "x"})).is_none());
    }
}

#[cfg(test)]
mod html_tests {
    use super::*;

    #[test]
    fn parses_huodongxing_list_item() {
        let html = r##"<div class="search-tab-content-item flex">
<a class="item-title" href="/event/2880642408800?utm_source=x" target="_blank">第四届人工智能生成内容国际会议 (AIGC 2026)</a>
<p class="item-data flex"><span class="item-data-icon icon"></span>2026.11.10-2026.11.12</p>
<p class="item-dress flex"><span class="item-dress-icon icon"></span>广东广州广交会展馆C区</p>
</div>"##;
        let items = parse_hdx_list(html);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "hdx-2880642408800");
        assert!(items[0].title.contains("AIGC"));
        assert_eq!(items[0].location.as_deref(), Some("广东广州广交会展馆C区"));
        assert!(items[0].start > 0);
        assert!(items[0].end.unwrap() > items[0].start);
    }

}
