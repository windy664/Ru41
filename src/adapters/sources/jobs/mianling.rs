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
        parse_jobs_page(&body, url).context("mianling parse failed")
    }
}

/// 解析岗位表格行；第一行是表头（首列"公司"）跳过。
fn parse_jobs_page(html: &str, page_url: &str) -> Result<Vec<FeedItem>> {
    let row_re = regex::Regex::new(r#"(?s)<tr[^>]*data-slot="table-row"[^>]*>(.*?)</tr>"#)
        .expect("row regex");
    let rows = row_re.captures_iter(html).map(|c| c[1].to_owned());
    let cell_re = regex::Regex::new(r"(?s)<t[dh][^>]*>(.*?)</t[dh]>").expect("cell regex");
    let tag_re = regex::Regex::new(r"<[^>]+>").expect("tag regex");
    // 公司列混着标签文本（"美团世界500强知名互联网"），按已知标签词切开
    let company_tag_re =
        regex::Regex::new(r"世界500强|知名互联网|新势力车企|半导体大厂|国企|央企|外企")
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
        let (company, job, city, audience, kind, posted, verified) = (
            cells[0].clone(),
            cells[1].clone(),
            cells[2].clone(),
            cells[3].clone(),
            cells[4].clone(),
            cells[5].clone(),
            cells[6].clone(),
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
        let id_key = format!("{company}:{job}:{city}");
        items.push(FeedItem {
            id: format!("mianling-{:x}", fnv1a(&id_key)),
            title: format!("{company} · {job}"),
            link: page_url.to_owned(),
            summary: format!("{kind} · {city} · 对象 {audience} · 发布 {posted} · 核实 {verified}"),
            published: posted,
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
        let items = parse_jobs_page(FIXTURE, "https://www.mianlingai.com/jobs/").unwrap();
        assert_eq!(items.len(), 2, "表头被跳过");
        assert_eq!(items[0].title, "美团 · 美团平台-数据开发工程师");
        assert!(items[0].summary.contains("校招"));
        assert!(items[0].summary.contains("北京"));
        assert_eq!(items[0].published, "10月25日");
        assert!(items[1].summary.contains("实习"));
        assert!(items[1].id.starts_with("mianling-"));
    }
}
