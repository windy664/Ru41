//! LFX Mentorship（mentorship.lfx.linuxfoundation.org，Linux 基金会导师计划）：
//! 公开 JSON API（api.mentorship.lfx.linuxfoundation.org/projects，免认证）。
//! 分页是 nextPageKey 游标式（page/pageSize 均被忽略，每页固定 ~9 条），
//! 服务端支持 acceptApplications=true 过滤——全库 1300+ 项目里开放的仅个位数，
//! 直接带过滤参数拉取，再客户端兜底复核。在 rss 模块里以
//! `lfx+https://api.mentorship.lfx.linuxfoundation.org/projects` 前缀接入。
//! 只收 acceptApplications 为 true 的项目。

use super::super::feed::{FeedItem, fetch_text, strip_tags};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`lfx+https://api.mentorship.lfx.linuxfoundation.org/projects`。
pub const URL_PREFIX: &str = "lfx+";

/// 注册表条目：`lfx+<api-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { LfxClient::new().fetch_projects(&url).await }),
};

/// 游标翻页封顶，防止 API 行为异常时死循环。
const MAX_PAGES: usize = 5;

pub struct LfxClient {
    http: reqwest::Client,
}

impl Default for LfxClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LfxClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 拉取开放申请的项目：带 acceptApplications=true 过滤，游标翻页至空或封顶。
    pub async fn fetch_projects(&self, url: &str) -> Result<Vec<FeedItem>> {
        let mut items = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut req = self
                .http
                .get(url)
                .query(&[("pageSize", "100"), ("acceptApplications", "true")]);
            if let Some(key) = &cursor {
                req = req.query(&[("nextPageKey", key.as_str())]);
            }
            let body = fetch_text(req, "lfx").await?;
            let page = parse_page(&body, crate::util::now()).context("lfx json parse failed")?;
            items.extend(page.items);
            match page.next_page_key {
                Some(key) => cursor = Some(key),
                None => break,
            }
        }
        Ok(items)
    }
}

struct LfxPage {
    items: Vec<FeedItem>,
    next_page_key: Option<String>,
}

/// 解析一页 projects：只保留 acceptApplications==true（服务端过滤外的兜底复核）。
/// published 用抓取当天而非 createdOn：开放申请的项目多为创建多年的老项目，
/// 用 createdOn 会被 rss 的 max_age_days 时效过滤整体丢弃，源等于失效；
/// createdOn 挪进 summary 保留信息。
fn parse_page(body: &str, now: i64) -> Result<LfxPage> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let today = crate::util::today_utc(now);
    let items = v
        .get("projects")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|p| map_project(p, &today))
        .collect();
    let next_page_key = v
        .get("nextPageKey")
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Ok(LfxPage {
        items,
        next_page_key,
    })
}

fn str_field(p: &serde_json::Value, key: &str) -> String {
    p.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned()
}

fn term_name(t: &serde_json::Value) -> &str {
    t.get("name").and_then(|n| n.as_str()).unwrap_or("")
}

/// programTerms 数组里 open 期的名称；没有 open 期则回退全部期名。
fn term_names(p: &serde_json::Value) -> String {
    let terms: Vec<&serde_json::Value> = p
        .get("programTerms")
        .and_then(|a| a.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let open: Vec<&str> = terms
        .iter()
        .filter(|t| t.get("active").and_then(|a| a.as_str()) == Some("open"))
        .map(|t| term_name(t))
        .filter(|s| !s.is_empty())
        .collect();
    let names = if open.is_empty() {
        terms
            .iter()
            .map(|t| term_name(t))
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        open
    };
    names.join(", ")
}

fn map_project(p: &serde_json::Value, today: &str) -> Option<FeedItem> {
    if !p.get("acceptApplications").and_then(|b| b.as_bool())? {
        return None;
    }
    let id = p.get("projectId")?.as_str()?;
    let name = str_field(p, "name");
    if name.is_empty() {
        return None;
    }
    let mut parts = vec![];
    let lf = str_field(p, "lfProjectName");
    if !lf.is_empty() {
        parts.push(lf);
    }
    let industry = str_field(p, "industry");
    if !industry.is_empty() {
        parts.push(industry);
    }
    let terms = term_names(p);
    if !terms.is_empty() {
        parts.push(format!("期次 {terms}"));
    }
    // createdOn 形如 "2025-02-10 19:56:08 +0000"，取日期部分。
    let created = str_field(p, "createdOn")
        .chars()
        .take(10)
        .collect::<String>();
    if !created.is_empty() {
        parts.push(format!("创建于 {created}"));
    }
    let desc = str_field(p, "description");
    if !desc.is_empty() {
        // description 带 HTML 标签（<p> 等），去标签后再截断。
        parts.push(crate::util::truncate_chars(&strip_tags(&desc), 300));
    }
    Some(FeedItem {
        id: format!("lfx-{id}"),
        title: format!("LFX Mentorship：{name}"),
        link: format!("https://mentorship.lfx.linuxfoundation.org/project/{id}"),
        summary: parts.join(" · "),
        published: today.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "projects": [
        {
          "projectId": "80719aac-15fd-428a-8a4c-7963e71b469d",
          "name": "LF Energy - Hyphae APIS",
          "status": "Published",
          "industry": "Java,JavaScript,Python",
          "description": "Hyphae APIS 是一个微服务网格示例项目，用于演示分布式电力系统调度。",
          "acceptApplications": true,
          "lfProjectName": "Hyphae",
          "createdOn": "2025-02-10 19:56:08 +0000",
          "programTerms": [
            {"name": "UNPAID - FALL 2025", "active": "closed"},
            {"name": "UNPAID - SPRING 2026", "active": "open"}
          ]
        },
        {
          "projectId": "4eb229ab-ebba-486b-a3c0-0b533871ef65",
          "name": "Jaeger Mentorship",
          "acceptApplications": false,
          "lfProjectName": "Cloud Native Computing Foundation (CNCF)",
          "createdOn": "2019-02-20 15:57:50 +0000"
        }
      ],
      "nextPageKey": "eyJwYWdlIjoyfQ=="
    }"#;

    #[test]
    fn keeps_only_accepting_projects() {
        // 2026-10-26T00:00:00Z
        let page = parse_page(FIXTURE, 1_792_972_800).unwrap();
        assert_eq!(page.items.len(), 1, "acceptApplications=false 的被过滤");
        assert_eq!(page.next_page_key.as_deref(), Some("eyJwYWdlIjoyfQ=="));
        let it = &page.items[0];
        assert_eq!(it.id, "lfx-80719aac-15fd-428a-8a4c-7963e71b469d");
        assert_eq!(it.title, "LFX Mentorship：LF Energy - Hyphae APIS");
        assert_eq!(
            it.link,
            "https://mentorship.lfx.linuxfoundation.org/project/80719aac-15fd-428a-8a4c-7963e71b469d"
        );
        assert!(it.summary.contains("Hyphae"));
        assert!(it.summary.contains("Java,JavaScript,Python"));
        assert!(
            it.summary.contains("期次 UNPAID - SPRING 2026"),
            "{}",
            it.summary
        );
        assert!(it.summary.contains("创建于 2025-02-10"), "{}", it.summary);
        assert!(it.summary.contains("微服务网格示例项目"));
        // published 用抓取当天：老项目的 createdOn 会被 max_age_days 时效过滤丢弃。
        assert_eq!(it.published, "2026-10-26");
    }

    #[test]
    fn falls_back_to_all_term_names_when_none_open() {
        let body = r#"{"projects":[{"projectId":"x","name":"无 open 期","acceptApplications":true,
            "programTerms":[{"name":"Spring 2027","active":"closed"}]}],"nextPageKey":""}"#;
        let page = parse_page(body, 1_792_972_800).unwrap();
        assert!(page.next_page_key.is_none(), "空游标即翻页结束");
        assert!(page.items[0].summary.contains("期次 Spring 2027"));
    }
}
