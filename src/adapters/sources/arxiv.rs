use crate::adapters::sources::feed::resolve_ref;
use anyhow::{Context, Result, bail};
use quick_xml::{Reader, events::Event};

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
        let http = reqwest::Client::builder()
            .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build");
        Self {
            http,
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
/// quick-xml 把实体引用拆成独立事件，文本按标签累积后在 </tag> 时落字段。
pub fn parse_feed(xml: &str) -> Result<Vec<ArxivEntry>> {
    let mut reader = Reader::from_str(xml);
    let mut entries = Vec::new();
    let mut current: Option<PartialEntry> = None;
    let mut buf = String::new();
    let mut in_author = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let name = e.local_name();
                match name.as_ref() {
                    "entry" => current = Some(PartialEntry::default()),
                    "author" if current.is_some() => in_author = true,
                    _ => buf.clear(),
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "link"
                    && let Some(entry) = current.as_mut()
                {
                    let mut href = None;
                    let mut is_pdf = false;
                    for attr in e.attributes().flatten() {
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
                            entry.pdf_url = Some(href);
                        } else {
                            entry.abs_url = href;
                        }
                    }
                }
            }
            Event::Text(e) => {
                if current.is_some() {
                    buf.push_str(&e.xml10_content());
                }
            }
            Event::CData(e) => {
                if current.is_some() {
                    buf.push_str(&e.xml10_content());
                }
            }
            Event::GeneralRef(e) => {
                if current.is_some()
                    && let Some(resolved) = resolve_ref(&e)
                {
                    buf.push_str(&resolved);
                }
            }
            Event::End(e) => {
                let name = e.local_name();
                let name = name.as_ref();
                match name {
                    "entry" => {
                        if let Some(partial) = current.take() {
                            entries.push(partial.finish()?);
                        }
                    }
                    "author" => in_author = false,
                    _ => {
                        if let Some(entry) = current.as_mut() {
                            let text = buf.trim();
                            if !text.is_empty() {
                                if in_author && name == "name" {
                                    entry.authors.push(text.to_owned());
                                } else {
                                    match name {
                                        "id" => entry.id = text.to_owned(),
                                        "title" => entry.title = text.to_owned(),
                                        "summary" => entry.summary = text.to_owned(),
                                        "published" => entry.published = text.to_owned(),
                                        "updated" => entry.updated = text.to_owned(),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        buf.clear();
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(entries)
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
}

impl PartialEntry {
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
