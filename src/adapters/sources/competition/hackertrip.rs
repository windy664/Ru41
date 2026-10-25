//! HackerTrip（hackertrip.space）：中文黑客松聚合平台，自带公开 JSON API
//! （/api/hackathons，免认证），字段齐全——城市/线上线下/奖金池/报名截止/
//! 主办方官网链接都有。在 rss 模块里以 `hackertrip+<api-url>` 前缀接入。
//! 只收 status 为 upcoming / live 的赛事；closed 的丢弃。
//! 该站还维护 llms.txt 和 .well-known/agent.json（A2A 协议），
//! 组队类接口要登录，我们只蹭它的公开赛事流。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`hackertrip+https://hackertrip.space/api/hackathons`。
pub const URL_PREFIX: &str = "hackertrip+";

/// 注册表条目：`hackertrip+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { HackerTripClient::new().fetch_hackathons(&url).await }),
};

pub struct HackerTripClient {
    http: reqwest::Client,
}

impl Default for HackerTripClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HackerTripClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_hackathons(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(self.http.get(url), "hackertrip").await?;
        parse_events(&body).context("hackertrip json parse failed")
    }
}

/// 解析 /api/hackathons：JSON 数组，只保留 upcoming / live。
fn parse_events(body: &str) -> Result<Vec<FeedItem>> {
    let events: Vec<serde_json::Value> = serde_json::from_str(body)?;
    Ok(events.iter().filter_map(map_event).collect())
}

fn map_event(ev: &serde_json::Value) -> Option<FeedItem> {
    let status = ev.get("status")?.as_str()?;
    if status != "upcoming" && status != "live" {
        return None;
    }
    let id = ev.get("id")?.as_str()?;
    let name = ev.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    // website 是主办方官方报名页；缺失时回退到站内探索页
    let website = ev
        .get("website")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("https://hackertrip.space/explore")
        .to_owned();
    let city = ev.get("city").and_then(|v| v.as_str()).unwrap_or("");
    let mode = match ev.get("mode").and_then(|v| v.as_str()) {
        Some("online") => "线上",
        Some("offline") => "线下",
        Some("hybrid") => "混合",
        _ => "",
    };
    let mut summary_parts = vec![];
    let place = [city, mode]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("·");
    if !place.is_empty() {
        summary_parts.push(place);
    }
    if let Some(p) = ev.get("prizePool").and_then(|v| v.as_str())
        && !p.is_empty()
    {
        // prizePool 常是长段描述，截断保留关键信息
        let p = crate::util::truncate_chars(p, 60);
        summary_parts.push(format!("奖池 {p}"));
    }
    let deadline = ev
        .get("registrationDeadline")
        .and_then(|v| v.as_str())
        .and_then(|s| s.get(..10))
        .unwrap_or("");
    if !deadline.is_empty() {
        summary_parts.push(format!("报名截止 {deadline}"));
    }
    if status == "live" {
        summary_parts.push("进行中".to_owned());
    }
    Some(FeedItem {
        id: format!("hackertrip-{id}"),
        title: name.to_owned(),
        link: website,
        summary: summary_parts.join(" · "),
        published: deadline.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      {
        "id": "ht-2027",
        "name": "雾客松 2027（WAKEATHON）重庆跨年 AI 黑客松",
        "city": "重庆",
        "mode": "hybrid",
        "status": "upcoming",
        "prizePool": "总奖池超 20 万元：冠军 8 万元现金+奖杯",
        "registrationDeadline": "2027-01-10T15:59:59.000Z",
        "website": "https://example.com/signup"
      },
      {
        "id": "ht-live-1",
        "name": "支付宝AI付 PAYATHON 2026",
        "city": "上海",
        "mode": "hybrid",
        "status": "live",
        "prizePool": "总奖池 ¥200,000",
        "registrationDeadline": "2026-12-01T00:00:00.000Z",
        "website": "https://aipay.alipay.com"
      },
      {
        "id": "ht-old",
        "name": "已结束的赛事",
        "status": "closed",
        "website": "https://example.com/old"
      }
    ]"#;

    #[test]
    fn keeps_upcoming_and_live_only() {
        let items = parse_events(FIXTURE).unwrap();
        assert_eq!(items.len(), 2, "closed 的被过滤");
        assert_eq!(items[0].id, "hackertrip-ht-2027");
        assert_eq!(items[0].title, "雾客松 2027（WAKEATHON）重庆跨年 AI 黑客松");
        assert_eq!(items[0].link, "https://example.com/signup");
        assert!(items[0].summary.contains("重庆·混合"));
        assert!(items[0].summary.contains("奖池 总奖池超 20 万元"));
        assert!(items[0].summary.contains("报名截止 2027-01-10"));
        assert!(items[1].summary.contains("进行中"));
    }

    #[test]
    fn falls_back_to_explore_when_no_website() {
        let body = r#"[{"id":"x","name":"无官网","status":"upcoming"}]"#;
        let items = parse_events(body).unwrap();
        assert_eq!(items[0].link, "https://hackertrip.space/explore");
    }
}
