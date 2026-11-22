//! php.net 官方发布（Atom）+ ChangeLog —— PHP 的安全修复在 ChangeLog 里。
//!
//! 为什么不能直接订阅 php.net 的 Atom(2026-11-22 实测):
//! 条目只有 `PHP 8.5.13 released!` / `There is a new PHP release in town!` ——
//! **空洞公告**（没有任何漏洞信息）。真正的安全修复在
//! `https://www.php.net/ChangeLog-8.php` 里，形如：
//!
//! ```text
//! Fixed GHSA-8mjj-2r7p-4hg2 (Use-after-free in magic method backtraces can
//! lead to remote code execution). (CVE-2026-129474)
//! ```
//!
//! 而发布公告页(php.net/releases/8_5_13.php)有一句关键信息：
//! 「This is a security release. All PHP 8.5 users are encouraged to upgrade.」
//!
//! 所以本适配器做三件事：
//! 1. 抓发布 Atom，只留带 `<php:tag>security</php:tag>` 的条目；
//! 2. 抓一次 ChangeLog，把该版本里带 GHSA-/CVE- 的条目抠出来；
//! 3. 抓发布公告页，抠出「建议升级」的受影响版本范围。
//!
//! 以 `php+https://www.php.net/releases/feed.php` 前缀接入。

use super::super::feed::{FeedItem, fetch_text, strip_tags};
use anyhow::Result;

/// rss feed 列表里的接入前缀。
pub const URL_PREFIX: &str = "php+";

/// 注册表条目：`php+<feed-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { PhpClient::new().fetch_releases(&url).await }),
};

/// ChangeLog 页面（按大版本分文件；8.x 是现在的主线）。
const CHANGELOG_URL: &str = "https://www.php.net/ChangeLog-8.php";

pub struct PhpClient {
    http: reqwest::Client,
}

impl Default for PhpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl PhpClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_releases(&self, url: &str) -> Result<Vec<FeedItem>> {
        let body = fetch_text(self.http.get(url), "php").await?;
        let entries = parse_feed(&body);
        let security_entries: Vec<ReleaseEntry> =
            entries.into_iter().filter(|e| e.security).collect();
        if security_entries.is_empty() {
            return Ok(vec![]);
        }
        // ChangeLog 整个页面较大，一轮只抓一次。
        let changelog = fetch_text(self.http.get(CHANGELOG_URL), "php-changelog")
            .await
            .unwrap_or_default();
        let mut items = Vec::new();
        for entry in security_entries {
            let fixes = extract_security_fixes(&changelog, &entry.version);
            let affected = fetch_affected_note(&self.http, &entry.announcement).await;
            let mut facts: Vec<String> = Vec::new();
            facts.push(format!("PHP {} 安全发布", entry.version));
            if let Some(note) = affected {
                facts.push(note);
            }
            if fixes.is_empty() {
                // 空洞公告：没有具体安全条目就不成条（见 plugins/rss 的空洞判据）。
                // 这里直接不产出条目 —— 宁可不报，不要报一句空话。
                continue;
            }
            facts.push(format!("{} 个安全修复", fixes.len()));
            for f in fixes.iter().take(20) {
                facts.push(f.clone());
            }
            items.push(FeedItem {
                id: format!("php-{}", entry.version),
                title: format!(
                    "PHP {} 安全发布：{} 个安全修复（含 GHSA/CVE 编号）",
                    entry.version,
                    fixes.len()
                ),
                link: entry.announcement,
                summary: facts.join(" · "),
                published: entry.published,
            });
        }
        Ok(items)
    }
}

/// Atom 条目里我们关心的字段。
#[derive(Debug, Clone)]
struct ReleaseEntry {
    version: String,
    published: String,
    announcement: String,
    security: bool,
}

/// 解析 php.net 的发布 Atom：只取带 php:tag security 的条目。
///
/// XML 标签用 `local_name()` 取（去命名空间前缀）：`php:version` → `version`、
/// `php:tag` → `tag`，与 feed.rs 的解析方式一致。
fn parse_feed(body: &str) -> Vec<ReleaseEntry> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);

    let mut entries: Vec<ReleaseEntry> = Vec::new();
    let mut cur: Option<ReleaseEntry> = None;
    let mut in_tag = false;
    let mut content_src = String::new();
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.local_name().as_ref().to_owned();
                match name.as_str() {
                    "entry" => {
                        cur = Some(ReleaseEntry {
                            version: String::new(),
                            published: String::new(),
                            announcement: String::new(),
                            security: false,
                        });
                        content_src.clear();
                    }
                    "version" | "published" | "tag" => {
                        if cur.is_some() {
                            text.clear();
                            in_tag = true;
                        }
                    }
                    "content" => {
                        content_src.clear();
                        for attr in e.attributes().flatten() {
                            if attr.key.local_name().as_ref() == "src" {
                                content_src = attr.value.to_string();
                            }
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(t)) => {
                if in_tag {
                    text.push_str(&t.xml10_content());
                }
            }
            Ok(Event::End(e)) => {
                let name = e.local_name().as_ref().to_owned();
                match name.as_str() {
                    "entry" => {
                        if let Some(mut entry) = cur.take() {
                            if entry.version.is_empty() {
                                // 没有版本号就退回公告页地址里的版本号。
                                entry.version = content_src
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or_default()
                                    .trim_end_matches(".php")
                                    .replace('_', ".");
                            }
                            if entry.announcement.is_empty() {
                                entry.announcement = format!(
                                    "https://www.php.net/releases/{}.php",
                                    entry.version.replace('.', "_")
                                );
                            }
                            entries.push(entry);
                        }
                        content_src.clear();
                    }
                    "version" | "published" | "tag" => {
                        if in_tag && let Some(entry) = cur.as_mut() {
                            if name == "published" {
                                // 只留日期部分(RFC3339 的时分秒交给下游也行,
                                // 但统一成 YYYY-MM-DD 便于按日分组与比较)。
                                entry.published = text.trim().chars().take(10).collect();
                            } else if name == "version" && entry.version.is_empty() {
                                entry.version = text.trim().to_owned();
                            } else if name == "tag" && text.trim().eq_ignore_ascii_case("security")
                            {
                                entry.security = true;
                            }
                        }
                        in_tag = false;
                    }
                    "content" => {
                        if let Some(entry) = cur.as_mut()
                            && !content_src.is_empty()
                        {
                            let mut link = content_src.clone();
                            if link.starts_with("http://php.net/") {
                                link = link.replacen("http://php.net/", "https://www.php.net/", 1);
                            }
                            entry.announcement = link;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    entries
}

/// 从 ChangeLog 文本里抠出某个版本的安全修复条目。
///
/// ChangeLog 的结构是「Version 8.5.13」开头一段，到下一个「Version 」为止；
/// 段内每个修复是若干行，安全修复那几行里带 GHSA- 或 CVE- 编号。
fn extract_security_fixes(changelog: &str, version: &str) -> Vec<String> {
    if changelog.is_empty() {
        return vec![];
    }
    let section = extract_version_section(changelog, version);
    let lines: Vec<&str> = section
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !(line.contains("GHSA-") || line.contains("CVE-")) {
            i += 1;
            continue;
        }
        // 描述通常在下一行（以 "(" 开头），带上它才算「有实质内容」；
        // 吃掉这一行，别让描述行里的 CVE- 编号再算一条。
        let mut entry = line.to_owned();
        let mut consumed = 1;
        if let Some(next) = lines.get(i + 1)
            && next.starts_with('(')
        {
            entry.push(' ');
            entry.push_str(next);
            consumed = 2;
        }
        if !out.contains(&entry) {
            out.push(entry);
        }
        i += consumed;
    }
    out
}

/// 取出「Version <version>」到下一个「Version 」之间的正文。
fn extract_version_section(changelog: &str, version: &str) -> String {
    let marker = format!("Version {version}");
    let Some(start) = changelog.find(&marker) else {
        return String::new();
    };
    let rest = &changelog[start + marker.len()..];
    let end = rest.find("Version ").unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// 抓发布公告页，抠出「建议升级」的受影响版本那句。
async fn fetch_affected_note(http: &reqwest::Client, url: &str) -> Option<String> {
    let body = fetch_text(http.get(url), "php-release").await.ok()?;
    let text = strip_tags(&body);
    let idx = text.find("This is a security release.")?;
    let tail = text[idx + "This is a security release.".len()..].trim();
    let end = tail.find('.').map(|p| p + 1).unwrap_or(tail.len().min(300));
    let note = tail[..end].trim().trim_end_matches('.').trim();
    if note.is_empty() {
        None
    } else {
        Some(format!("受影响版本提示：{note}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <feed xmlns="http://www.w3.org/2005/Atom" xmlns:php="http://php.net/ns/releases">
    <entry>
        <title>PHP 8.5.13 released!</title>
        <id>http://php.net/releases/8_5_13.php</id>
        <php:version>8.5.13</php:version>
        <published>2026-11-19T00:00:00+00:00</published>
        <summary type="html">There is a new PHP release in town!</summary>
        <content src="http://php.net/releases/8_5_13.php" type="application/xhtml+xml"/>
        <php:tag>security</php:tag>
    </entry>
    <entry>
        <title>PHP 8.6.0 released!</title>
        <id>http://php.net/releases/8_6_0.php</id>
        <php:version>8.6.0</php:version>
        <published>2026-11-19T00:00:00+00:00</published>
        <summary type="html">There is a new PHP release in town!</summary>
        <content src="http://php.net/releases/8_6_0.php" type="application/xhtml+xml"/>
    </entry>
    </feed>"#;

    #[test]
    fn keeps_only_security_tagged_releases() {
        let entries = parse_feed(FEED);
        assert_eq!(entries.len(), 2);
        let sec = &entries[0];
        assert!(sec.security, "带 php:tag security 的应被识别");
        assert_eq!(sec.version, "8.5.13");
        assert_eq!(sec.published, "2026-11-19");
        assert!(
            sec.announcement
                .starts_with("https://www.php.net/releases/"),
            "{}",
            sec.announcement
        );
        assert!(!entries[1].security, "没打 security 标签的普通发布");
    }

    #[test]
    fn extracts_security_fixes_from_changelog() {
        let changelog = r#"Version 8.5.13
19 Nov 2026
Core:
Fixed bug
GH-24184
(System ID not being reinitialized).
Fixed
GHSA-8mjj-2r7p-4hg2
(Use-after-free in magic method backtraces can lead to remote code execution). (CVE-2026-129474)
Version 8.5.12
Something else
"#;
        let fixes = extract_security_fixes(changelog, "8.5.13");
        assert_eq!(fixes.len(), 1, "{fixes:?}");
        assert!(fixes[0].contains("GHSA-8mjj-2r7p-4hg2"), "{}", fixes[0]);
        assert!(fixes[0].contains("CVE-2026-129474"), "{}", fixes[0]);
        assert!(fixes[0].contains("Use-after-free"), "{}", fixes[0]);
    }

    #[test]
    fn no_fixes_means_no_entry() {
        let changelog = "Version 8.5.13\nCore:\nFixed bug\nGH-1\n(nothing security)\n";
        assert!(extract_security_fixes(changelog, "8.5.13").is_empty());
        assert!(extract_security_fixes("", "8.5.13").is_empty());
    }
}
