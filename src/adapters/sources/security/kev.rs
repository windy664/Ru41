//! CISA 已知被利用漏洞目录（KEV）—— **已在野利用**的那一档。
//!
//! 数据来自 CISA 的 JSON 目录（免认证、公开）：`vulnerabilities[]` 每条带
//! cveID / vendorProject / product / vulnerabilityName / dateAdded /
//! shortDescription / requiredAction / dueDate / knownRansomwareCampaignUse。
//!
//! 这是「现在就修」的权威口径：进了 KEV 说明**已经有真实攻击在用**，
//! 且有联邦机构的强制处置期限(dueDate)。日报里这类条目必须单独报。
//!
//! 目录是**累积**的（全库 1700+ 条），所以这里只取近期新增的
//! （默认 7 天；URL 上可用 `days=` 覆盖），否则每天会把全库重报一遍。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀。
pub const URL_PREFIX: &str = "kev+";

/// 注册表条目：`kev+<json-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { KevClient::new().fetch_recent(&url).await }),
};

/// 近期窗口（天）：只报「这几天新进目录的」。
const DEFAULT_DAYS: u32 = 7;

pub struct KevClient {
    http: reqwest::Client,
}

impl Default for KevClient {
    fn default() -> Self {
        Self::new()
    }
}

impl KevClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_recent(&self, url: &str) -> Result<Vec<FeedItem>> {
        let (base, days) = super::split_days(url, DEFAULT_DAYS);
        let body = fetch_text(self.http.get(&base), "kev").await?;
        let items = parse_kev(&body, days).context("kev json parse failed")?;
        Ok(items)
    }
}
/// 解析 KEV 目录：只取 dateAdded 落在窗口内的。
fn parse_kev(body: &str, days: u32) -> Result<Vec<FeedItem>> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let arr = v
        .get("vulnerabilities")
        .and_then(|a| a.as_array())
        .context("kev 响应没有 vulnerabilities 数组")?
        .clone();
    let cutoff = crate::util::today_utc(crate::util::now() - (days as i64) * 86_400);
    let mut items = Vec::new();
    for entry in arr {
        let Some(item) = map_entry(&entry, &cutoff) else {
            continue;
        };
        items.push(item);
    }
    // 新增的排前面（同日多条时按编号稳定排序）。
    items.sort_by(|a, b| b.published.cmp(&a.published).then(a.id.cmp(&b.id)));
    Ok(items)
}

fn map_entry(e: &serde_json::Value, cutoff: &str) -> Option<FeedItem> {
    let cve = e.get("cveID")?.as_str()?.trim().to_owned();
    if cve.is_empty() {
        return None;
    }
    let added = e
        .get("dateAdded")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    // 只收窗口内新增的（目录是累积的）。
    if added.len() < 10 || added.as_str() < cutoff {
        return None;
    }
    let vendor = e
        .get("vendorProject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let product = e
        .get("product")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let name = e
        .get("vulnerabilityName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let desc = e
        .get("shortDescription")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let action = e
        .get("requiredAction")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let due = e
        .get("dueDate")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let ransomware = e
        .get("knownRansomwareCampaignUse")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    let mut facts: Vec<String> = Vec::new();
    facts.push("CISA 已知被利用漏洞目录（KEV）".to_owned());
    if !vendor.is_empty() || !product.is_empty() {
        facts.push(format!("受影响：{vendor} {product}").trim().to_owned());
    }
    if !desc.is_empty() {
        facts.push(crate::util::truncate_chars(desc, 400));
    }
    if !action.is_empty() {
        facts.push(format!("要求动作：{action}"));
    }
    if !due.is_empty() {
        facts.push(format!("处置期限 {due}"));
    }
    if !ransomware.is_empty() && !ransomware.eq_ignore_ascii_case("unknown") {
        facts.push(format!("勒索软件使用情况：{ransomware}"));
    }

    Some(FeedItem {
        id: format!("kev-{cve}"),
        title: format!("{cve}：{name}"),
        link: format!("https://nvd.nist.gov/vuln/detail/{cve}"),
        summary: facts.join(" · "),
        published: added,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "title": "CISA Catalog of Known Exploited Vulnerabilities",
      "catalogVersion": "2026.11.20",
      "count": 2,
      "vulnerabilities": [
        {
          "cveID": "CVE-2026-99999",
          "vendorProject": "Acme",
          "product": "Widget",
          "vulnerabilityName": "Acme Widget RCE",
          "dateAdded": "2026-11-20",
          "shortDescription": "Acme widgets allow remote code execution without authentication.",
          "requiredAction": "Apply mitigations per vendor instructions.",
          "dueDate": "2026-12-01",
          "knownRansomwareCampaignUse": "Known"
        },
        {
          "cveID": "CVE-2020-29583",
          "vendorProject": "Zyxel",
          "product": "Multiple Products",
          "vulnerabilityName": "Zyxel hard-coded credentials",
          "dateAdded": "2021-11-03",
          "shortDescription": "hard-coded credential",
          "requiredAction": "n/a",
          "dueDate": "2022-05-03",
          "knownRansomwareCampaignUse": "Unknown"
        }
      ]
    }"#;

    #[test]
    fn keeps_only_recent_additions() {
        let items = parse_kev(FIXTURE, 7).unwrap();
        assert_eq!(items.len(), 1, "老条目不该每天重报: {items:?}");
        let it = &items[0];
        assert_eq!(it.id, "kev-CVE-2026-99999");
        assert!(it.title.contains("Acme Widget RCE"));
        assert!(it.summary.contains("KEV"), "{}", it.summary);
        assert!(it.summary.contains("无需认证") || it.summary.contains("without authentication"));
        assert!(it.summary.contains("处置期限 2026-12-01"), "{}", it.summary);
        assert!(it.summary.contains("勒索软件"), "{}", it.summary);
        assert_eq!(it.published, "2026-11-20");
    }

    #[test]
    fn drops_old_entries_with_wide_window_off() {
        // 窗口 0 天：只收今天新增的。
        let items = parse_kev(FIXTURE, 0).unwrap();
        assert!(items.is_empty(), "{items:?}");
    }
}
