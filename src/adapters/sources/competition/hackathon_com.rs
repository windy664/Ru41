//! 黑客松.com（xn--ubtw9y6z8b.com）：全球 AI 黑客松赛事库。
//!
//! 公开 JSON 接口（免鉴权），数据由其开源仓库
//! （atomgit.com/hackathon-com/hackathon-site）多通道抓取、逐字段核验后发布，
//! 只放出核验完成的场次。端点：
//!
//! - `api/search.json`：全量索引（968 条，含已结束）。
//! - `api/closing.json`：14 天内截止窗口（67 条）。
//!
//! 接口**不支持查询参数**（country/mode 等都被忽略），筛选一律在本地做。
//! 以 `hackathoncom+<api-url>` 接入，与其他伪 feed 源同构。
//!
//! 字段口径：`deadline` 是报名/投稿截止（`deadline_kind` 区分两者），
//! `lifecycle` 是场次状态（recruiting/ongoing/closed/ended），
//! `prize` 可能是奖金额也可能是「Goodies + Certificate」这类非现金描述，
//! `prizes_note` 是中文细则（可含「非现金：…」说明）。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};
use serde::Deserialize;

/// rss feed 列表里的接入前缀：`hackathoncom+<api-url>`。
pub const URL_PREFIX: &str = "hackathoncom+";

/// 全量索引（含已结束场次，筛选在本地做）。
pub const SEARCH_URL: &str = "https://xn--ubtw9y6z8b.com/api/search.json";
/// 14 天内截止窗口（体量小，适合机会雷达的日常推送）。
pub const CLOSING_URL: &str = "https://xn--ubtw9y6z8b.com/api/closing.json";

/// 注册表条目：`hackathoncom+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { HackathonComClient::new().fetch_items(&url).await }),
};

/// 参赛资格（受众）信息。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Eligibility {
    /// 谁能参加（如「AIGC 创作方向创作者（海选报名制）」）。
    pub who: Option<String>,
    /// 队伍要求（如「2-5 人」）。
    pub team: Option<String>,
}

impl Eligibility {
    /// 受众文本（who + team），用于中小学赛判定。
    pub fn audience(&self) -> String {
        [self.who.as_deref(), self.team.as_deref()]
            .into_iter()
            .flatten()
            .filter(|part| !part.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// null → 空串：接口偶尔给 null（如某场的 `register_url`），
/// 不能让整份数据解析失败、把这个源整个丢掉。
fn null_to_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// 类型化赛事条目：雷达映射成 FeedItem，竞赛日程总览直接用字段。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HackathonComEvent {
    #[serde(deserialize_with = "null_to_empty")]
    pub id: String,
    #[serde(deserialize_with = "null_to_empty")]
    pub name: String,
    /// 主办方（可能很长，含支持方名单）。
    #[serde(deserialize_with = "null_to_empty")]
    pub organizer: String,
    /// 赛事详情页。
    #[serde(deserialize_with = "null_to_empty")]
    pub url: String,
    /// 报名入口（比详情页更直接）；接口可能给 null。
    pub register_url: Option<String>,
    /// 截止时间（ISO 8601，带时区）。
    pub deadline: Option<String>,
    /// 截止口径：registration（报名截止）/ submission（投稿截止）/ rolling 等。
    pub deadline_kind: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    /// online / offline / hybrid。
    pub mode: Option<String>,
    /// 城市（纯线上场次为空）。
    pub city: Option<String>,
    /// cn / overseas（纯线上常为空）。
    pub country: Option<String>,
    /// 奖金（可能是金额，也可能是非现金描述）。
    pub prize: Option<String>,
    /// 奖金细则（中文，含「非现金：…」等说明）。
    pub prizes_note: Option<String>,
    /// recruiting / ongoing / closed / ended / unknown。
    pub lifecycle: Option<String>,
    /// 核验置信度：high / medium。
    pub confidence: Option<String>,
    /// 一句话答案（是什么/谁办/何时截止）。
    pub answer: Option<String>,
    /// 领域分类（如「AI/大模型」「艺术/创意」）。
    pub domains: Vec<String>,
    /// 自由标签。
    pub tags: Vec<String>,
    /// 赛道。
    pub tracks: Vec<String>,
    pub eligibility: Eligibility,
    /// 站方发布门槛：只发布核验完成的场次。
    pub publish_ready: bool,
}

impl HackathonComEvent {
    /// 线上或混合：可远程参与。
    pub fn is_online(&self) -> bool {
        matches!(self.mode.as_deref(), Some("online") | Some("hybrid"))
    }

    /// 场次是否还没结束（ended/unknown 不再是机会）。
    pub fn is_open(&self) -> bool {
        !matches!(self.lifecycle.as_deref(), Some("ended") | Some("unknown"))
    }

    /// 报名/投稿入口优先，其次详情页。
    pub fn link(&self) -> String {
        self.register_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| self.url.clone())
    }

    /// 城市 + 形态的中文标注。
    pub fn place_label(&self) -> String {
        let mode = match self.mode.as_deref() {
            Some("online") => "线上",
            Some("offline") => "线下",
            Some("hybrid") => "混合",
            _ => "",
        };
        let city = self.city.clone().unwrap_or_default();
        if city.is_empty() {
            // 纯线上场次没有城市，形态本身就是地点。
            return if mode.is_empty() {
                "线上".to_owned()
            } else {
                mode.to_owned()
            };
        }
        if mode.is_empty() {
            city
        } else {
            format!("{city}·{mode}")
        }
    }

    /// 奖金文本：金额优先，细则补充（供有奖闸门判定）。
    pub fn prize_text(&self) -> String {
        let mut text = self.prize.clone().unwrap_or_default();
        if let Some(note) = &self.prizes_note
            && !note.trim().is_empty()
            && !text.contains(note.trim())
        {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(note.trim());
        }
        text
    }

    /// 题材/领域（领域在前，标签补位）。
    pub fn theme_labels(&self) -> Vec<String> {
        let mut themes: Vec<String> = self
            .domains
            .iter()
            .chain(self.tracks.iter())
            .filter(|item| !item.trim().is_empty())
            .map(|item| crate::util::truncate_chars(item, 20))
            .collect();
        themes.dedup();
        for tag in &self.tags {
            if themes.len() >= 4 {
                break;
            }
            let tag = crate::util::truncate_chars(tag, 16);
            if !tag.is_empty() && !themes.contains(&tag) {
                themes.push(tag);
            }
        }
        themes
    }

    fn to_feed_item(&self) -> FeedItem {
        let mut parts = vec![self.place_label()];
        if let Some(organizer) = self.organizer.split('（').next() {
            let organizer = organizer.trim();
            if !organizer.is_empty() {
                parts.push(organizer.to_owned());
            }
        }
        let prize = self.prize.clone().unwrap_or_default();
        if !prize.is_empty() {
            parts.push(crate::util::truncate_chars(&prize, 40));
        }
        if let Some(deadline) = &self.deadline {
            parts.push(format!("截止 {}", deadline));
        }
        if let Some(answer) = &self.answer {
            parts.push(crate::util::truncate_chars(answer, 120));
        }
        FeedItem {
            id: format!("hackathoncom-{}", self.id),
            title: self.name.clone(),
            link: self.link(),
            summary: parts.join(" · "),
            published: self
                .deadline
                .as_deref()
                .and_then(|value| value.get(0..10))
                .unwrap_or_default()
                .to_owned(),
        }
    }
}

/// 接口返回体：结果在 `results` 里，`total` 是全库条数（closing 端点也带它，
/// 别拿它当本次返回条数）。
#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    results: Vec<HackathonComEvent>,
}

pub struct HackathonComClient {
    http: reqwest::Client,
}

impl Default for HackathonComClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HackathonComClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 抓取并转成 feed 条目（rss 模块的伪 feed 入口）。
    pub async fn fetch_items(&self, url: &str) -> Result<Vec<FeedItem>> {
        Ok(self
            .fetch_events(url)
            .await?
            .iter()
            .map(HackathonComEvent::to_feed_item)
            .collect())
    }

    /// 抓取类型化条目。
    ///
    /// 站点偶发 TLS 断连（实测请求间隔密集时会出现），这里重试 3 次；
    /// 仍失败就交给上层按「单源失败降级」处理，不影响其他源。
    pub async fn fetch_events(&self, url: &str) -> Result<Vec<HackathonComEvent>> {
        let mut last: Option<anyhow::Error> = None;
        for attempt in 0..3 {
            match self.fetch_once(url).await {
                Ok(events) => return Ok(events),
                Err(error) => {
                    last = Some(error);
                    tokio::time::sleep(std::time::Duration::from_millis(800 * (attempt + 1))).await;
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("hackathon.com fetch failed")))
    }

    async fn fetch_once(&self, url: &str) -> Result<Vec<HackathonComEvent>> {
        let body = fetch_text(self.http.get(url), "hackathon.com").await?;
        let response: ApiResponse =
            serde_json::from_str(&body).context("hackathon.com json parse failed")?;
        Ok(response
            .results
            .into_iter()
            .filter(|event| event.publish_ready)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "updated_at": "2026-11-10T22:49:34+08:00",
      "total": 3,
      "results": [
        {
          "id": "hk-20261030-2814",
          "name": "Injective Hacker House 曼谷（首届）",
          "organizer": "Injective（HackQuest 联合组织）",
          "url": "https://injectivehackerhouse.com/",
          "register_url": null,
          "deadline": "2026-11-10T23:59:00+08:00",
          "deadline_kind": "registration",
          "start": "2026-12-01T00:00:00+08:00",
          "end": "2026-12-10T23:59:00+08:00",
          "mode": "offline",
          "city": "Bangkok",
          "country": "overseas",
          "prize": null,
          "prizes_note": "非现金：入选团队获得全程住宿与餐食、差旅补贴、AWS 云资源",
          "lifecycle": "recruiting",
          "confidence": "high",
          "answer": "曼谷线下黑客松，11-10 报名截止。",
          "domains": ["AI/大模型"],
          "tags": ["曼谷"],
          "tracks": [],
          "eligibility": {"who": "全球团队", "team": null},
          "publish_ready": true
        },
        {
          "id": "hk-2",
          "name": "昇腾 Triton 算子开发挑战赛",
          "organizer": "昇腾社区",
          "url": "https://example.com/triton",
          "register_url": "https://example.com/triton/register",

          "deadline": "2026-11-14T23:59:00+08:00",
          "deadline_kind": "submission",
          "start": "2026-11-20T09:00:00+08:00",
          "mode": "online",
          "city": null,
          "country": "cn",
          "prize": "40000 元",
          "prizes_note": null,
          "lifecycle": "ongoing",
          "confidence": "high",
          "answer": "线上算子挑战赛。",
          "domains": ["AI/大模型"],
          "tags": ["昇腾", "算子"],
          "tracks": ["AI/大模型"],
          "eligibility": {"who": null, "team": "1-3 人"},
          "publish_ready": true
        },
        {
          "id": "hk-3",
          "name": "未核验场次",
          "organizer": "某人",
          "url": "https://example.com/x",
          "deadline": "2026-11-14T23:59:00+08:00",
          "mode": "online",
          "lifecycle": "ended",
          "publish_ready": false
        }
      ]
    }"#;

    #[test]
    fn parses_events_and_drops_unpublished() {
        let response: ApiResponse = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(response.results.len(), 3);
        let first = &response.results[0];
        assert_eq!(first.name, "Injective Hacker House 曼谷（首届）");
        assert_eq!(first.deadline_kind.as_deref(), Some("registration"));
        assert_eq!(first.place_label(), "Bangkok·线下");
        assert!(!first.is_online());
        assert!(first.is_open());
        // 报名入口为空时回落到详情页
        assert_eq!(first.link(), "https://injectivehackerhouse.com/");
        // 奖金字段缺省时用细则补位
        assert!(first.prize_text().contains("差旅补贴"));

        let second = &response.results[1];
        assert_eq!(second.link(), "https://example.com/triton/register");
        assert!(second.is_online());
        assert_eq!(second.place_label(), "线上");
        assert_eq!(second.prize_text(), "40000 元");
        assert_eq!(second.theme_labels(), vec!["AI/大模型", "昇腾", "算子"]);
        assert_eq!(second.eligibility.audience(), "1-3 人");

        // publish_ready=false 的场次不发布
        assert!(!response.results[2].publish_ready);
        assert!(!response.results[2].is_open());
    }

    #[test]
    fn feed_item_carries_deadline_and_place() {
        let response: ApiResponse = serde_json::from_str(FIXTURE).unwrap();
        let item = response.results[1].to_feed_item();
        assert_eq!(item.id, "hackathoncom-hk-2");
        assert_eq!(item.published, "2026-11-14");
        assert!(item.summary.contains("线上"));
        assert!(item.summary.contains("截止 2026-11-14T23:59:00+08:00"));
    }
}
