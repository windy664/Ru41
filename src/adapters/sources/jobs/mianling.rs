//! 面灵 AI 校招/实习岗位表（mianlingai.com/jobs）：每日核实的大厂岗位聚合，
//! SSR 直接出表格（无需渲染）：公司 | 岗位 | 城市 | 招聘对象 | 类型 | 发布 | 最后核实。
//! 以 `mianling+<page-url>` 前缀接入雷达。校招/实习都收，取舍交给 digest。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`mianling+https://www.mianlingai.com/jobs/`。
pub const URL_PREFIX: &str = "mianling+";

/// 注册表条目：`mianling+<page-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { MianlingClient::new().fetch_jobs(&url).await }),
};

pub struct MianlingClient {
    http: reqwest::Client,
}

impl Default for MianlingClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MianlingClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_jobs(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(self.http.get(url), "mianling").await?;
        let jobs = parse_jobs_page(&body).context("mianling parse failed")?;
        Ok(jobs
            .into_iter()
            .map(|job| job.into_feed_item(url))
            .collect())
    }

    /// 结构化抓取：modules/jobs 的岗位总览需要分列字段，不走 FeedItem 拼串。
    pub async fn fetch_jobs_struct(&self, url: &str) -> Result<Vec<MianlingJob>> {
        let body = fetch_text(self.http.get(url), "mianling").await?;
        parse_jobs_page(&body).context("mianling parse failed")
    }

    /// tRPC 公开接口：campusJob.list 按 employmentType（"实习"/"校招"）取最新 50 条，
    /// 比 SSR 表格多了 platform（回源平台）与精确发布时间。翻页需登录，只取第 1 页。
    pub async fn fetch_latest(&self, employment_type: &str) -> Result<Vec<MianlingJob>> {
        #[derive(serde::Deserialize)]
        struct Trpc {
            result: TrpcResult,
        }
        #[derive(serde::Deserialize)]
        struct TrpcResult {
            data: TrpcData,
        }
        #[derive(serde::Deserialize)]
        struct TrpcData {
            json: TrpcJson,
        }
        #[derive(serde::Deserialize)]
        struct TrpcJson {
            items: Vec<ApiItem>,
        }
        #[derive(serde::Deserialize)]
        struct ApiItem {
            company: String,
            title: String,
            cities: Vec<String>,
            batch: Option<String>,
            #[serde(rename = "employmentType")]
            employment_type: String,
            #[serde(rename = "publishedAt")]
            published_at: Option<String>,
            platform: Option<String>,
        }
        let input = serde_json::json!({"json": {"page": 1, "pageSize": 50, "employmentType": employment_type}});
        let url = format!(
            "https://api.aceround.app/trpc/campusJob.list?input={}",
            urlencoding(&input.to_string())
        );
        let trpc: Trpc = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(trpc
            .result
            .data
            .json
            .items
            .into_iter()
            .map(|item| MianlingJob {
                company: item.company,
                role: item.title,
                city: if item.cities.is_empty() {
                    "未公布".into()
                } else {
                    item.cities.join(" / ")
                },
                audience: item.batch.unwrap_or_else(|| "未公布".into()),
                kind: item.employment_type,
                posted: item
                    .published_at
                    .as_deref()
                    .map(iso_to_md)
                    .unwrap_or_else(|| "未公布".into()),
                platform: item.platform,
            })
            .collect())
    }
}

/// 最小 URL query 编码（按 UTF-8 字节编码，中文也要正确成 %XX%XX%XX）。
fn urlencoding(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// ISO 时间戳 → "10月25日"（UTC+8，与面灵页面展示一致）。
fn iso_to_md(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| {
            let local = dt.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap());
            format!("{}月{}日", local.format("%m").to_string().trim_start_matches('0'), local.format("%d").to_string().trim_start_matches('0'))
        })
        .unwrap_or_else(|_| "未公布".into())
}

/// 一条岗位记录（公司/岗位/城市/对象/类型/发布日/回源平台）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MianlingJob {
    pub company: String,
    pub role: String,
    pub city: String,
    pub audience: String,
    pub kind: String,
    pub posted: String,
    /// 回源平台（ashby/greenhouse/feishu-hire/...）；SSR 表格拿不到，为 None。
    pub platform: Option<String>,
}

impl MianlingJob {
    fn into_feed_item(self, page_url: &str) -> FeedItem {
        let id_key = format!("{}:{}:{}", self.company, self.role, self.city);
        FeedItem {
            id: format!("mianling-{:x}", fnv1a(&id_key)),
            title: format!("{} · {}", self.company, self.role),
            link: page_url.to_owned(),
            summary: format!(
                "{} · {} · 对象 {} · 发布 {}",
                self.kind, self.city, self.audience, self.posted
            ),
            published: self.posted,
        }
    }
}

/// 解析岗位表格行；第一行是表头（首列"公司"）跳过。
fn parse_jobs_page(html: &str) -> Result<Vec<MianlingJob>> {
    let row_re = regex::Regex::new(r#"(?s)<tr[^>]*data-slot="table-row"[^>]*>(.*?)</tr>"#)
        .expect("row regex");
    let rows = row_re.captures_iter(html).map(|c| c[1].to_owned());
    let cell_re = regex::Regex::new(r"(?s)<t[dh][^>]*>(.*?)</t[dh]>").expect("cell regex");
    let tag_re = regex::Regex::new(r"<[^>]+>").expect("tag regex");
    // 公司列混着标签文本（"美团世界500强知名互联网"），按已知标签词切开
    let company_tag_re = regex::Regex::new(
        r"世界500强|知名互联网|新势力车企|半导体大厂|大模型公司|独角兽|上市公司|创业公司|高新技术企业|专精特新|小巨人|国企|央企|外企",
    )
    .expect("company tag regex");
    let mut items = Vec::new();
    for row in rows {
        let cells: Vec<String> = cell_re
            .captures_iter(&row)
            .map(|c| {
                let text = tag_re.replace_all(&c[1], "");
                text.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .collect();
        if cells.len() < 8 || cells[0] == "公司" {
            continue;
        }
        let (company, job, city, audience, kind, posted) = (
            cells[0].clone(),
            cells[1].clone(),
            cells[2].clone(),
            cells[3].clone(),
            cells[4].clone(),
            cells[5].clone(),
        );
        if job.is_empty() {
            continue;
        }
        let company = company_tag_re
            .split(&company)
            .next()
            .unwrap_or(&company)
            .trim()
            .to_owned();
        items.push(MianlingJob {
            company,
            role: job,
            city,
            audience,
            kind,
            posted,
            platform: None,
        });
    }
    Ok(items)
}

/// 轻量稳定散列（与 xfyun 同款，无外部依赖）。
fn fnv1a(text: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for b in text.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
<tr data-slot="table-row"><td data-slot="table-cell">公司</td><td>岗位</td><td>城市</td><td>招聘对象</td><td>类型</td><td>发布</td><td>最后核实</td><td>投递</td></tr>
<tr data-slot="table-row" class="x"><td data-slot="table-cell"><span>美团</span><span>世界500强</span></td>
<td data-slot="table-cell">美团平台-数据开发工程师</td><td data-slot="table-cell">北京</td>
<td data-slot="table-cell">未公布</td><td data-slot="table-cell">校招</td><td data-slot="table-cell">10月25日</td>
<td data-slot="table-cell">10月26日</td><td data-slot="table-cell">登录后投递</td></tr>
<tr data-slot="table-row" class="x"><td data-slot="table-cell">滴滴知名互联网</td>
<td data-slot="table-cell">效能平台部-产品实习生</td><td data-slot="table-cell">北京</td>
<td data-slot="table-cell">未公布</td><td data-slot="table-cell">实习</td><td data-slot="table-cell">10月23日</td>
<td data-slot="table-cell">10月26日</td><td data-slot="table-cell">登录后投递</td></tr>
"#;

    #[test]
    fn parses_job_rows_and_skips_header() {
        let items = parse_jobs_page(FIXTURE).unwrap();
        assert_eq!(items.len(), 2, "表头被跳过");
        assert_eq!(items[0].company, "美团");
        assert_eq!(items[0].role, "美团平台-数据开发工程师");
        assert_eq!(items[0].kind, "校招");
        assert_eq!(items[0].city, "北京");
        assert_eq!(items[0].posted, "10月25日");
        assert_eq!(items[1].kind, "实习");

        let feeds: Vec<FeedItem> = items
            .into_iter()
            .map(|job| job.into_feed_item("https://www.mianlingai.com/jobs/"))
            .collect();
        assert_eq!(feeds[0].title, "美团 · 美团平台-数据开发工程师");
        assert!(feeds[0].summary.contains("校招"));
        assert!(feeds[0].id.starts_with("mianling-"));
    }
}
