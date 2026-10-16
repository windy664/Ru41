use anyhow::{Context, Result};
use quick_xml::{Reader, events::Event};

/// 一条订阅源资讯条目（RSS 2.0 或 Atom；摘要已去 HTML 标签并截断）。
#[derive(Debug, Clone, PartialEq)]
pub struct FeedItem {
    /// 条目唯一标识：guid/id 优先，缺省退回 link，再退回 title+published。
    pub id: String,
    pub title: String,
    pub link: String,
    pub summary: String,
    pub published: String,
}

/// 通用订阅源客户端（RSS 2.0 / Atom 自动识别）；每个源的抓取间隔由调用方控制。
pub struct FeedClient {
    http: reqwest::Client,
}

impl Default for FeedClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client build");
        Self { http }
    }

    pub async fn fetch(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = self
            .http
            .get(url)
            .send()
            .await
            .context("rss request failed")?
            .error_for_status()
            .context("rss feed returned error status")?
            .text()
            .await
            .context("rss body read failed")?;
        parse_feed(&body)
    }
}

/// 解析 RSS 2.0（<rss><channel><item>…）与 Atom（<feed><entry>…）两种格式。
/// 与 arxiv 解析同一模式：实体引用是独立事件，文本按标签累积后在 </tag> 落字段。
pub fn parse_feed(xml: &str) -> Result<Vec<FeedItem>> {
    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut current: Option<PartialItem> = None;
    let mut buf = String::new();
    let mut format = FeedFormat::Unknown;
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                "rss" => format = FeedFormat::Rss,
                "feed" => format = FeedFormat::Atom,
                "item" | "entry" => current = Some(PartialItem::default()),
                _ => buf.clear(),
            },
            Event::Empty(e) => {
                // Atom 的链接在 <link href="..."/> 属性上。
                if e.local_name().as_ref() == "link"
                    && let Some(entry) = current.as_mut()
                {
                    let mut href = None;
                    let mut is_alternate = true;
                    for attr in e.attributes().flatten() {
                        match attr.key.local_name().as_ref() {
                            "href" => {
                                href = Some(
                                    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                                        .into_owned(),
                                )
                            }
                            "rel" => is_alternate = attr.value.as_ref() == "alternate",
                            _ => {}
                        }
                    }
                    if let (Some(href), true) = (href, is_alternate) {
                        entry.link = href;
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
            Event::End(e) => match e.local_name().as_ref() {
                "item" | "entry" => {
                    if let Some(partial) = current.take() {
                        items.push(partial.finish());
                    }
                }
                _ => {
                    if let Some(item) = current.as_mut() {
                        item.set(e.local_name().as_ref(), buf.trim());
                    }
                    buf.clear();
                }
            },
            Event::Eof => break,
            _ => {}
        }
    }
    anyhow::ensure!(format != FeedFormat::Unknown, "not an RSS 2.0 or Atom feed");
    Ok(items)
}

/// 兼容旧名：parse_rss 即 parse_feed。
pub fn parse_rss(xml: &str) -> Result<Vec<FeedItem>> {
    parse_feed(xml)
}

#[derive(PartialEq)]
enum FeedFormat {
    Unknown,
    Rss,
    Atom,
}

#[derive(Default)]
struct PartialItem {
    /// RSS guid 或 Atom id。
    guid: String,
    title: String,
    link: String,
    /// RSS description 或 Atom summary/content。
    description: String,
    published: String,
}

impl PartialItem {
    fn set(&mut self, tag: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        match tag {
            "guid" | "id" => self.guid = text.to_owned(),
            "title" => self.title = text.to_owned(),
            "link" => {
                // RSS 的链接是文本节点；Atom 走 Empty 事件属性，不覆盖。
                if self.link.is_empty() {
                    self.link = text.to_owned();
                }
            }
            "description" | "summary" | "content" => self.description = text.to_owned(),
            "pubDate" | "date" | "published" => self.published = text.to_owned(),
            // Atom 的 updated 只作兜底，不覆盖已有 published。
            "updated" if self.published.is_empty() => self.published = text.to_owned(),
            _ => {}
        }
    }

    fn finish(self) -> FeedItem {
        let id = if !self.guid.is_empty() {
            self.guid.clone()
        } else if !self.link.is_empty() {
            self.link.clone()
        } else {
            format!("{}:{}", self.title, self.published)
        };
        FeedItem {
            id,
            title: self.title,
            link: self.link,
            summary: strip_html(&self.description),
            published: self.published,
        }
    }
}

/// description 常含 HTML：去标签、还原实体、压缩空白、截断到 2000 字符。
fn strip_html(html: &str) -> String {
    let tag = regex::Regex::new(r"<[^>]+>").expect("html tag regex");
    let text = tag.replace_all(html, " ");
    // CDATA 内容不经 XML 实体解析，去标签后手动反转义。
    let text = quick_xml::escape::unescape(&text)
        .map(|t| t.into_owned())
        .unwrap_or_else(|_| text.to_string());
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= 2000 {
        return text;
    }
    let mut out: String = text.chars().take(2000).collect();
    out.push('…');
    out
}

/// 解析实体引用：先预定义实体（&amp; 等），再数字字符引用（&#88; / &#x58;）。
/// 未知引用返回 None（丢弃该片段，不影响其余内容）。各 XML 来源解析共用。
pub(crate) fn resolve_ref(reference: &str) -> Option<String> {
    if let Some(s) = quick_xml::escape::resolve_predefined_entity(reference) {
        return Some(s.to_owned());
    }
    let num = reference.strip_prefix('#')?;
    let code = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => num.parse().ok()?,
    };
    char::from_u32(code).map(|c| c.to_string())
}
