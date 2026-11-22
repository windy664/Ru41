//! GitHub 官方 Advisory Database（GHSA）—— 结构化漏洞源。
//!
//! 数据来自 GitHub 公开 REST API `GET /advisories`：免认证、每分钟 60 次，
//! 支持 `sort=published&direction=desc` 与 `published` / `severity` / `ecosystem` 过滤。
//! 一条公告带：GHSA 编号、CVE 编号、CVSS 分值与向量、官方描述、严重等级。
//!
//! 以 `ghsa+https://api.github.com/advisories?...` 前缀接入；只收近期发布的
//! （默认 7 天，URL 上带 `days` 查询参数可改），避免把历史库当新料报。
//!
//! 2026-11-22 实测：`per_page=100&sort=published&direction=desc` 返回的条目里
//! 已有 `cvss: {score, vector_string}`（3.1/4.0 都有），`severity` 是
//! low/moderate/high/critical。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀。
pub const URL_PREFIX: &str = "ghsa+";

/// 注册表条目：`ghsa+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { GhsaClient::new().fetch_advisories(&url).await }),
};

/// 近期窗口（天）。URL 上可用 `days=` 覆盖。
const DEFAULT_DAYS: u32 = 7;

pub struct GhsaClient {
    http: reqwest::Client,
}

impl Default for GhsaClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GhsaClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 拉取近期发布的 advisory。
    ///
    /// URL 形如 `https://api.github.com/advisories?per_page=100`，
    /// 额外支持自定义键 `days=7` 覆盖时间窗（从 URL 里摘掉，不发给 GitHub）。
    pub async fn fetch_advisories(&self, url: &str) -> Result<Vec<FeedItem>> {
        let (base, days) = super::split_days(url, DEFAULT_DAYS);
        let mut req = self.http.get(&base);
        // 只在调用方没写死 per_page/sort 时补默认值。
        if !base.contains("per_page") {
            req = req.query(&[("per_page", "100")]);
        }
        if !base.contains("sort=") {
            req = req.query(&[("sort", "published"), ("direction", "desc")]);
        }
        let body = fetch_text(req, "ghsa").await?;
        let items = parse_advisories(&body, days).context("ghsa json parse failed")?;
        Ok(items)
    }
}
/// 解析 advisory 列表：只收近期发布、且有摘要的。
fn parse_advisories(body: &str, days: u32) -> Result<Vec<FeedItem>> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let arr = v.as_array().context("advisories 响应不是数组")?.clone();
    let cutoff = crate::util::now() - (days as i64) * 86_400;
    let mut items = Vec::new();
    for a in arr {
        let Some(item) = map_advisory(&a, cutoff) else {
            continue;
        };
        items.push(item);
    }
    Ok(items)
}

fn map_advisory(a: &serde_json::Value, cutoff: i64) -> Option<FeedItem> {
    let ghsa_id = a.get("ghsa_id")?.as_str()?.trim();
    if ghsa_id.is_empty() {
        return None;
    }
    let summary = a
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if summary.is_empty() {
        return None;
    }
    let published = a
        .get("published_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    // 只收近期发布的（published_at 是 RFC3339，取前十位是日期；用日期比秒级时间戳粗一点）。
    let published_day = published.chars().take(10).collect::<String>();
    let day_epoch = crate::util::today_utc(cutoff);
    if published_day.len() == 10 && published_day.as_str() < day_epoch.as_str() {
        // 日期字符串比较对 ISO 日期是等价的；早于窗口就丢。
        let too_old = published_day < day_epoch;
        if too_old {
            return None;
        }
    }
    let severity = a
        .get("severity")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let cvss = a.get("cvss");
    let cvss_score = cvss.and_then(|c| c.get("score")).and_then(|s| s.as_f64());
    let cvss_vector = cvss
        .and_then(|c| c.get("vector_string"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    let cve_id = a
        .get("cve_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let description = a
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let mut facts: Vec<String> = Vec::new();
    if !severity.is_empty() {
        facts.push(format!("严重等级 {severity}"));
    }
    if let Some(score) = cvss_score {
        facts.push(format!("CVSS {score}"));
    }
    if !cvss_vector.is_empty() {
        facts.push(format!("向量 {cvss_vector}"));
    }
    if !cve_id.is_empty() {
        facts.push(format!("对应 {cve_id}"));
    }
    let desc = crate::util::truncate_chars(
        description.lines().collect::<Vec<_>>().join(" ").trim(),
        600,
    );
    if !desc.is_empty() {
        facts.push(desc);
    }
    let link = a
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();

    Some(FeedItem {
        id: format!("ghsa-{ghsa_id}"),
        title: format!("{ghsa_id}：{summary}"),
        link,
        summary: facts.join(" · "),
        published: published_day,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"[
      {
        "ghsa_id": "GHSA-25qc-m4pw-2w8r",
        "cve_id": "CVE-2026-122247",
        "html_url": "https://github.com/advisories/GHSA-25qc-m4pw-2w8r",
        "summary": "Budibase: Unauthenticated global-admin & database creation via tenant-confusion bypass",
        "description": "Description:\n\nOn a self-hosted Budibase instance the public bootstrap endpoint is bypassable.",
        "type": "reviewed",
        "severity": "high",
        "published_at": "2026-11-20T23:50:01Z",
        "cvss": {"vector_string": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:H", "score": 8.2}
      },
      {
        "ghsa_id": "GHSA-old-old-old",
        "summary": "很久以前的公告",
        "published_at": "2020-01-01T00:00:00Z",
        "severity": "critical",
        "cvss": {"score": 9.8}
      }
    ]"#;

    #[test]
    fn maps_advisory_with_cvss_and_cve() {
        let items = parse_advisories(FIXTURE, 7).unwrap();
        assert_eq!(items.len(), 1, "超出时间窗的应被丢掉: {items:?}");
        let it = &items[0];
        assert_eq!(it.id, "ghsa-GHSA-25qc-m4pw-2w8r");
        assert!(it.title.contains("GHSA-25qc-m4pw-2w8r"));
        assert!(it.title.contains("Budibase"));
        assert!(it.summary.contains("严重等级 high"), "{}", it.summary);
        assert!(it.summary.contains("CVSS 8.2"), "{}", it.summary);
        assert!(it.summary.contains("CVE-2026-122247"), "{}", it.summary);
        assert!(it.summary.contains("PR:N"), "{}", it.summary);
        assert_eq!(it.published, "2026-11-20");
    }

    #[test]
    fn drops_advisory_without_summary() {
        let body = r#"[{"ghsa_id":"GHSA-x","published_at":"2026-11-20T00:00:00Z"}]"#;
        assert!(parse_advisories(body, 7).unwrap().is_empty());
    }
}
