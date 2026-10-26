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

/// 类型化赛事条目：雷达映射成 FeedItem，竞赛日程总览直接用结构体字段。
pub struct HackertripEvent {
    pub id: String,
    pub name: String,
    pub city: String,
    /// online / offline / hybrid
    pub mode: String,
    pub status: String,
    pub prize_pool: String,
    /// ISO 日期（yyyy-mm-dd），空串表示无
    pub registration_deadline: String,
    pub start_date: String,
    pub website: String,
    pub theme: String,
    pub tracks: Vec<String>,
}

impl HackertripEvent {
    /// 雷达候选条目形态：摘要拼接城市/奖池/截止等关键字段。
    fn to_feed_item(&self) -> FeedItem {
        let mode_cn = match self.mode.as_str() {
            "online" => "线上",
            "offline" => "线下",
            "hybrid" => "混合",
            _ => "",
        };
        let mut summary_parts = vec![];
        let place = [self.city.as_str(), mode_cn]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("·");
        if !place.is_empty() {
            summary_parts.push(place);
        }
        if !self.prize_pool.is_empty() {
            let p = crate::util::truncate_chars(&self.prize_pool, 60);
            summary_parts.push(format!("奖池 {p}"));
        }
        if !self.registration_deadline.is_empty() {
            summary_parts.push(format!("报名截止 {}", self.registration_deadline));
        }
        if self.status == "live" {
            summary_parts.push("进行中".to_owned());
        }
        FeedItem {
            id: format!("hackertrip-{}", self.id),
            title: self.name.clone(),
            link: self.website.clone(),
            summary: summary_parts.join(" · "),
            published: self.registration_deadline.clone(),
        }
    }
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
        Ok(self
            .fetch_events(url)
            .await?
            .iter()
            .map(HackertripEvent::to_feed_item)
            .collect())
    }

    /// 拉全量并解析为类型化条目；只保留 upcoming / live。
    pub async fn fetch_events(&self, url: &str) -> Result<Vec<HackertripEvent>> {
        let body = fetch_text(self.http.get(url), "hackertrip").await?;
        parse_events(&body).context("hackertrip json parse failed")
    }
}

/// 解析 /api/hackathons：JSON 数组，只保留 upcoming / live。
fn parse_events(body: &str) -> Result<Vec<HackertripEvent>> {
    let events: Vec<serde_json::Value> = serde_json::from_str(body)?;
    Ok(events.iter().filter_map(map_event).collect())
}

fn str_field(ev: &serde_json::Value, key: &str) -> String {
    ev.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn map_event(ev: &serde_json::Value) -> Option<HackertripEvent> {
    let status = ev.get("status")?.as_str()?;
    if status != "upcoming" && status != "live" {
        return None;
    }
    let id = ev.get("id")?.as_str()?;
    let name = str_field(ev, "name");
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
    let tracks = ev
        .get("tracks")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("title").and_then(|v| v.as_str()))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Some(HackertripEvent {
        id: id.to_owned(),
        name,
        city: str_field(ev, "city"),
        mode: str_field(ev, "mode"),
        status: status.to_owned(),
        prize_pool: str_field(ev, "prizePool"),
        registration_deadline: str_field(ev, "registrationDeadline")
            .chars()
            .take(10)
            .collect(),
        start_date: str_field(ev, "startDate").chars().take(10).collect(),
        website,
        theme: str_field(ev, "theme"),
        tracks,
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
        "website": "https://example.com/signup",
        "tracks": [{"title": "开放式命题", "description": ""}]
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
        let first = &items[0];
        assert_eq!(first.id, "ht-2027");
        assert_eq!(first.name, "雾客松 2027（WAKEATHON）重庆跨年 AI 黑客松");
        assert_eq!(first.website, "https://example.com/signup");
        assert_eq!(first.registration_deadline, "2027-01-10");
        assert_eq!(first.tracks, vec!["开放式命题".to_owned()]);
        let item = first.to_feed_item();
        assert_eq!(item.id, "hackertrip-ht-2027");
        assert!(item.summary.contains("重庆·混合"));
        assert!(item.summary.contains("奖池 总奖池超 20 万元"));
        assert!(item.summary.contains("报名截止 2027-01-10"));
        assert!(items[1].to_feed_item().summary.contains("进行中"));
    }

    #[test]
    fn falls_back_to_explore_when_no_website() {
        let body = r#"[{"id":"x","name":"无官网","status":"upcoming"}]"#;
        let items = parse_events(body).unwrap();
        assert_eq!(items[0].website, "https://hackertrip.space/explore");
    }
}
