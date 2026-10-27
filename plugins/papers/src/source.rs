use anyhow::{Context, Result, bail};

/// 一篇 arXiv 论文的元数据（来自 Atom feed，摘要级，不含全文）。
#[derive(Debug, Clone, PartialEq)]
pub struct ArxivEntry {
    /// 规范 arXiv ID（去版本号），如 "2510.12345"。
    pub id: String,
    pub version: u32,
    pub title: String,
    pub summary: String,
    pub authors: Vec<String>,
    pub published: String,
    pub updated: String,
    pub abs_url: String,
    pub pdf_url: Option<String>,
}

/// arXiv 公共 API 客户端；限速 1 次/3 秒由调用方的轮询节奏保证。
#[derive(Clone)]
pub struct ArxivClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for ArxivClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ArxivClient {
    pub fn new() -> Self {
        Self::with_base_url("https://export.arxiv.org/api/query")
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: ru41::util::http_client(),
            base_url: base_url.into(),
        }
    }

    /// 按 arXiv ID 取单篇论文（用于精读等定向查询）；不存在返回 None。
    pub async fn fetch_by_id(&self, id: &str) -> Result<Option<ArxivEntry>> {
        let url = format!("{}?id_list={id}", self.base_url);
        let body = self
            .http
            .get(&url)
            .send()
            .await
            .context("arxiv api request failed")?
            .error_for_status()
            .context("arxiv api returned error status")?
            .text()
            .await
            .context("arxiv api body read failed")?;
        Ok(parse_feed(&body)?.into_iter().next())
    }

    /// 拉取指定分类的最新提交，按提交时间倒序。
    pub async fn fetch_new(
        &self,
        categories: &[String],
        max_results: u32,
    ) -> Result<Vec<ArxivEntry>> {
        anyhow::ensure!(!categories.is_empty(), "arxiv categories required");
        let query = categories
            .iter()
            .map(|c| format!("cat:{c}"))
            .collect::<Vec<_>>()
            .join("+OR+");
        let url = format!(
            "{}?search_query={query}&sortBy=submittedDate&sortOrder=descending&max_results={max_results}",
            self.base_url
        );
        let body = self
            .http
            .get(&url)
            .send()
            .await
            .context("arxiv api request failed")?
            .error_for_status()
            .context("arxiv api returned error status")?
            .text()
            .await
            .context("arxiv api body read failed")?;
        parse_feed(&body)
    }
}

/// 解析 Atom feed；容忍条目内字段顺序与多余命名空间。
/// 事件骨架与 RSS 解析共用 adapters::sources::feed::parse_items。
pub fn parse_feed(xml: &str) -> Result<Vec<ArxivEntry>> {
    ru41::adapters::sources::feed::parse_items::<PartialEntry>(
        xml,
        &["feed"],
        &["entry"],
        "not an arXiv Atom feed",
    )
}

#[derive(Default)]
struct PartialEntry {
    id: String,
    title: String,
    summary: String,
    authors: Vec<String>,
    published: String,
    updated: String,
    abs_url: String,
    pdf_url: Option<String>,
    in_author: bool,
}

impl ru41::adapters::sources::feed::XmlEntry for PartialEntry {
    type Output = ArxivEntry;

    fn on_start(&mut self, tag: &str) {
        if tag == "author" {
            self.in_author = true;
        }
    }

    fn on_empty(&mut self, tag: &str, element: &quick_xml::events::BytesStart) -> Result<()> {
        if tag != "link" {
            return Ok(());
        }
        let mut href = None;
        let mut is_pdf = false;
        for attr in element.attributes().flatten() {
            match attr.key.local_name().as_ref() {
                "href" => {
                    href = Some(
                        attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                            .into_owned(),
                    );
                }
                "title" => is_pdf = attr.value.as_ref() == "pdf",
                _ => {}
            }
        }
        if let Some(href) = href {
            if is_pdf {
                self.pdf_url = Some(href);
            } else {
                self.abs_url = href;
            }
        }
        Ok(())
    }

    fn on_end(&mut self, tag: &str, text: &str) {
        if tag == "author" {
            self.in_author = false;
            return;
        }
        if text.is_empty() {
            return;
        }
        if self.in_author && tag == "name" {
            self.authors.push(text.to_owned());
            return;
        }
        match tag {
            "id" => self.id = text.to_owned(),
            "title" => self.title = text.to_owned(),
            "summary" => self.summary = text.to_owned(),
            "published" => self.published = text.to_owned(),
            "updated" => self.updated = text.to_owned(),
            _ => {}
        }
    }

    fn finish(self) -> Result<ArxivEntry> {
        // id 形如 http://arxiv.org/abs/2510.12345v2
        let raw = self
            .id
            .rsplit('/')
            .next()
            .context("arxiv entry id missing")?
            .to_owned();
        let (id, version) = match raw.rsplit_once('v') {
            Some((base, v)) if !base.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => {
                (base.to_owned(), v.parse()?)
            }
            _ => (raw, 1),
        };
        if self.title.is_empty() {
            bail!("arxiv entry {id} missing title");
        }
        Ok(ArxivEntry {
            id,
            version,
            title: self.title,
            summary: self.summary,
            authors: self.authors,
            published: self.published,
            updated: self.updated,
            abs_url: if self.abs_url.is_empty() {
                self.id.clone()
            } else {
                self.abs_url
            },
            pdf_url: self.pdf_url,
        })
    }
}
