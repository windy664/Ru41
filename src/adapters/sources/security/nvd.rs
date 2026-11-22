//! NVD CVE API 2.0 —— 只取 CRITICAL（全部严重度每天上千条，会淹掉日报）。
//!
//! 数据来自 `services.nvd.nist.gov/rest/json/cves/2.0`：免认证、
//! 限流 5 次/30 秒（带 key 可到 50 次/30 秒）。
//!
//! 2026-11-22 实测：`pubStartDate` 近 2 天 = **1117** 条（全部严重度），
//! 加上 `cvssV3Severity=CRITICAL` = **6** 条。取 CRITICAL 才够用。
//!
//! NVD 的价值是**评分口径统一**（CVSS v3.1/v4.0 都有 baseScore 与向量），
//! 缺点是描述常常是英文模板句、且更新滞后。所以它排在 GHSA 后面当补充。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀。
pub const URL_PREFIX: &str = "nvd+";

/// 注册表条目：`nvd+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { NvdClient::new().fetch_critical(&url).await }),
};

/// 近期窗口（天）。
const DEFAULT_DAYS: u32 = 2;
/// 单次最多取多少条（API 单页上限 2000，这里留余量）。
const MAX_ITEMS: usize = 60;

pub struct NvdClient {
    http: reqwest::Client,
}

impl Default for NvdClient {
    fn default() -> Self {
        Self::new()
    }
}

impl NvdClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_critical(&self, url: &str) -> Result<Vec<FeedItem>> {
        let (base, days) = super::split_days(url, DEFAULT_DAYS);
        let end = crate::util::now();
        let start = end - (days as i64) * 86_400;
        let req = self.http.get(&base).query(&[
            ("pubStartDate", iso_utc(start)),
            ("pubEndDate", iso_utc(end)),
            ("cvssV3Severity", "CRITICAL".to_owned()),
            ("resultsPerPage", "200".to_owned()),
        ]);
        let body = fetch_text(req, "nvd").await?;
        let items = parse_cves(&body).context("nvd json parse failed")?;
        Ok(items.into_iter().take(MAX_ITEMS).collect())
    }
}

/// Unix 秒 → RFC3339(UTC)：`2026-11-20T10:00:00.000`（NVD API 要这个格式）。
fn iso_utc(ts: i64) -> String {
    let secs_of_day = ts.rem_euclid(86_400);
    let date = crate::util::today_utc(ts);
    format!(
        "{date}T{:02}:{:02}:{:02}.000",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}
/// 解析 CVE 列表：取 CVSS 评分最高的一条指标。
fn parse_cves(body: &str) -> Result<Vec<FeedItem>> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let arr = v
        .get("vulnerabilities")
        .and_then(|a| a.as_array())
        .context("nvd 响应没有 vulnerabilities 数组")?
        .clone();
    let mut items = Vec::new();
    for entry in arr {
        let Some(cve) = entry.get("cve") else {
            continue;
        };
        let Some(item) = map_cve(cve) else {
            continue;
        };
        items.push(item);
    }
    // 评分高的排前面。
    items.sort_by(|a, b| {
        score_of(b)
            .partial_cmp(&score_of(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(items)
}

/// 从标题/摘要里把评分抠出来（用于排序）。
fn score_of(item: &FeedItem) -> f64 {
    item.summary
        .split("CVSS ")
        .nth(1)
        .and_then(|rest| rest.split([' ', '·']).next())
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn map_cve(cve: &serde_json::Value) -> Option<FeedItem> {
    let id = cve.get("id")?.as_str()?.trim().to_owned();
    if id.is_empty() {
        return None;
    }
    let desc = cve
        .get("descriptions")
        .and_then(|d| d.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|d| d.get("lang").and_then(|l| l.as_str()) == Some("en"))
                .or_else(|| arr.first())
        })
        .and_then(|d| d.get("value"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let published = cve
        .get("published")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .chars()
        .take(10)
        .collect::<String>();

    // 取评分最高的那条 CVSS 指标（v4.0 / v3.1 / v2 都可能在）。
    let metrics = cve.get("metrics").and_then(|m| m.as_object());
    let mut best: Option<(f64, String, String)> = None;
    if let Some(metrics) = metrics {
        for (kind, list) in metrics {
            if !kind.starts_with("cvss") {
                continue;
            }
            let Some(list) = list.as_array() else {
                continue;
            };
            for m in list {
                let Some(data) = m.get("cvssData") else {
                    continue;
                };
                let score = data
                    .get("baseScore")
                    .and_then(|s| s.as_f64())
                    .unwrap_or(0.0);
                let vector = data
                    .get("vectorString")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                let severity = data
                    .get("baseSeverity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                if best.as_ref().map(|(s, _, _)| score > *s).unwrap_or(true) {
                    best = Some((score, vector, severity));
                }
            }
        }
    }
    let (score, vector, severity) = best.unwrap_or((0.0, String::new(), String::new()));

    let mut facts: Vec<String> = Vec::new();
    if score > 0.0 {
        facts.push(format!("CVSS {score}"));
    }
    if !severity.is_empty() {
        facts.push(format!("严重等级 {severity}"));
    }
    if !vector.is_empty() {
        facts.push(format!("向量 {vector}"));
    }
    if !desc.is_empty() {
        facts.push(crate::util::truncate_chars(&desc, 500));
    }

    Some(FeedItem {
        id: format!("nvd-{id}"),
        title: format!("{id}：{}", crate::util::truncate_chars(&desc, 120)),
        link: format!("https://nvd.nist.gov/vuln/detail/{id}"),
        summary: facts.join(" · "),
        published,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "totalResults": 2,
      "vulnerabilities": [
        {"cve": {"id": "CVE-2026-11111",
          "published": "2026-11-20T10:00:00.000",
          "descriptions": [{"lang": "en", "value": "SQUID is vulnerable to HTTP request smuggling, allowing a remote attacker to perform requests."}],
          "metrics": {"cvssMetricV31": [{"cvssData": {"baseScore": 9.3, "baseSeverity": "CRITICAL", "vectorString": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:L/A:N"}}]}}},
        {"cve": {"id": "CVE-2026-22222",
          "published": "2026-11-21T10:00:00.000",
          "descriptions": [{"lang": "en", "value": "Another critical issue."}],
          "metrics": {"cvssMetricV40": [{"cvssData": {"baseScore": 9.8, "baseSeverity": "CRITICAL", "vectorString": "CVSS:4.0/AV:N/AC:L/AT:P/PR:N/UI:N/VC:H/VI:N/VA:N"}}]}}}
      ]
    }"#;

    #[test]
    fn maps_cve_and_sorts_by_score() {
        let items = parse_cves(FIXTURE).unwrap();
        assert_eq!(items.len(), 2);
        // 9.8 的排前面
        assert_eq!(items[0].id, "nvd-CVE-2026-22222");
        assert!(
            items[0].summary.contains("CVSS 9.8"),
            "{}",
            items[0].summary
        );
        assert!(
            items[0].summary.contains("CVSS:4.0"),
            "{}",
            items[0].summary
        );
        assert!(
            items[1].summary.contains("request smuggling"),
            "{}",
            items[1].summary
        );
        assert_eq!(items[0].published, "2026-11-21");
    }
}
