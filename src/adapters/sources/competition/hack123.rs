//! 黑123（hack123.henryht.sh）：中文黑客松导航，每周更新，收录量约等于
//! 其他中文聚合站的三倍（2026-10 实测 370 场、155 场开放报名）。
//! 无 RSS/API，但 llms.txt 就是结构化全量清单（站点主动为 LLM 公示）：
//! "Hackathons open for registration" 段每行一个赛事，字段以 · 分隔——
//! deadline/starts/地点/线上形态/奖金/受众/难度/题材齐全。以 `hack123+<llms-url>` 接入。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`hack123+https://hack123.henryht.sh/llms.txt`。
pub const URL_PREFIX: &str = "hack123+";

/// 注册表条目：`hack123+<llms-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { Hack123Client::new().fetch_hackathons(&url).await }),
};

/// 类型化赛事条目：雷达映射成 FeedItem，竞赛日程总览直接用字段。
#[derive(Debug, Clone)]
pub struct Hack123Event {
    pub slug: String,
    pub name: String,
    pub link: String,
    /// ISO 日期 yyyy-mm-dd
    pub deadline: String,
    pub starts: String,
    /// 城市/地点原文（可能为空=纯线上）
    pub place: String,
    /// online / in person / hybrid（原文）
    pub mode: String,
    /// "prize $67.5k" / "prize ¥50万" 原文
    pub prize: String,
    /// 受众段原文（如 "for students"、"for students aged 14–18"）
    pub audience: String,
    /// 题材段（如 "AI/Payments"）
    pub themes: String,
    /// 一行简介（截断）
    pub blurb: String,
}

impl Hack123Event {
    pub fn is_online(&self) -> bool {
        self.mode == "online"
    }

    fn to_feed_item(&self) -> FeedItem {
        let mut parts = vec![];
        if !self.place.is_empty() {
            parts.push(self.place.clone());
        }
        if !self.mode.is_empty() {
            parts.push(self.mode.clone());
        }
        if !self.prize.is_empty() {
            parts.push(self.prize.clone());
        }
        if !self.deadline.is_empty() {
            parts.push(format!("截止 {}", self.deadline));
        }
        if !self.blurb.is_empty() {
            parts.push(self.blurb.clone());
        }
        FeedItem {
            id: format!("hack123-{}", self.slug),
            title: self.name.clone(),
            link: self.link.clone(),
            summary: parts.join(" · "),
            published: self.deadline.clone(),
        }
    }
}

pub struct Hack123Client {
    http: reqwest::Client,
}

impl Default for Hack123Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Hack123Client {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    pub async fn fetch_hackathons(&self, url: &str) -> Result<Vec<FeedItem>> {
        Ok(self
            .fetch_events(url)
            .await?
            .iter()
            .map(Hack123Event::to_feed_item)
            .collect())
    }

    /// 拉 llms.txt 并解析"开放报名"段为类型化条目。
    pub async fn fetch_events(&self, url: &str) -> Result<Vec<Hack123Event>> {
        let body = fetch_text(self.http.get(url), "hack123").await?;
        parse_open_section(&body).context("hack123 llms.txt parse failed")
    }
}

/// 已知字段段关键词（· 分隔段里不属于地点的那些）。
const KNOWN_SEGMENTS: &[&str] = &[
    "online",
    "in person",
    "hybrid",
    "international",
    "domestic",
    "beginner-friendly",
    "intermediate",
    "advanced",
    "for students",
    "student-friendly",
    "professionals",
];

/// 解析 "Hackathons open for registration" 段（到下一个 ## 为止）。
fn parse_open_section(text: &str) -> Result<Vec<Hack123Event>> {
    let Some(start) = text.find("## Hackathons open for registration") else {
        anyhow::bail!("open section not found");
    };
    let rest = &text[start..];
    let end = rest[3..].find("\n## ").map(|i| i + 3).unwrap_or(rest.len());
    let section = &rest[..end];
    Ok(section.lines().filter_map(parse_line).collect())
}

/// 一行：`- [EN name / 中文名](url): deadline 2026-11-13 · starts ... · 地点 · online · prize $X · 题材. 简介`
fn parse_line(line: &str) -> Option<Hack123Event> {
    let re = regex::Regex::new(r"^- \[([^\]]+)\]\(([^)]+)\): (.+)$").expect("h123 line regex");
    let caps = re.captures(line)?;
    let name_raw = &caps[1];
    let link = caps[2].to_owned();
    let slug = link.rsplit('/').next().unwrap_or("").to_owned();
    if slug.is_empty() {
        return None;
    }
    // 双语名称取中文部分（"EN / 中文"），纯英文直接用
    let name = name_raw
        .rsplit_once(" / ")
        .map(|(_, cn)| cn.trim())
        .unwrap_or_else(|| name_raw.trim())
        .to_owned();
    let body = &caps[3];
    // 简介在第一个 ". " 之后（字段段里没有 ". "，日期/奖金都不含）
    let (fields, blurb) = match body.split_once(". ") {
        Some((f, b)) => (f, b.trim()),
        None => (body, ""),
    };
    let mut ev = Hack123Event {
        slug,
        name,
        link,
        deadline: String::new(),
        starts: String::new(),
        place: String::new(),
        mode: String::new(),
        prize: String::new(),
        audience: String::new(),
        themes: String::new(),
        blurb: crate::util::truncate_chars(blurb, 120),
    };
    for seg in fields.split(" · ").map(str::trim) {
        if let Some(d) = seg.strip_prefix("deadline ") {
            ev.deadline = d.to_owned();
        } else if let Some(s) = seg.strip_prefix("starts ") {
            ev.starts = s.to_owned();
        } else if seg.starts_with("prize ") {
            ev.prize = seg.to_owned();
        } else if seg.starts_with("for students") || seg.starts_with("student") {
            ev.audience = seg.to_owned();
        } else if seg == "online" || seg == "in person" || seg == "hybrid" {
            ev.mode = seg.to_owned();
        } else if seg.contains('/')
            && !KNOWN_SEGMENTS.contains(&seg)
            && !seg.chars().any(|c| c.is_ascii_digit())
        {
            // "AI/Payments" 形态的题材段
            ev.themes = seg.to_owned();
        } else if !KNOWN_SEGMENTS.contains(&seg) && ev.place.is_empty() {
            ev.place = seg.to_owned();
        }
    }
    Some(ev)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"# Hack123 (黑123)
## Hackathons open for registration

- [PayPal AI Hackathon 2026](https://hack123.henryht.sh/en/h/paypal-ai-hackathon-2026): deadline 2026-11-13 · starts 2026-10-03 · online · international · prize $67.5k · intermediate · AI/Payments. Combine PayPal payments with AI.
- [SpaceXAI Macau Hackathon 2026 / SpaceXAI 澳门黑客松 2026](https://hack123.henryht.sh/en/h/spacexai-macau-hackathon-2026): deadline 2026-11-12 · starts 2026-11-12 · Macau · in person · international · prize $10k · for students · AI. 澳门线下.
- [Global Virtual Hackathon 2026（国际学生线上黑客松）](https://hack123.henryht.sh/en/h/global-virtual-hackathon-2026): deadline 2026-11-13 · online · for students aged 14–18 · beginner-friendly. K12 性质.

## Past hackathons
- [Old One](https://hack123.henryht.sh/en/h/old): deadline 2020-01-01.
"#;

    #[test]
    fn parses_open_section_only() {
        let events = parse_open_section(FIXTURE).unwrap();
        assert_eq!(events.len(), 3, "Past 段不混入");
        let pp = &events[0];
        assert_eq!(pp.slug, "paypal-ai-hackathon-2026");
        assert_eq!(pp.name, "PayPal AI Hackathon 2026");
        assert_eq!(pp.deadline, "2026-11-13");
        assert_eq!(pp.mode, "online");
        assert_eq!(pp.prize, "prize $67.5k");
        assert_eq!(pp.themes, "AI/Payments");
        assert_eq!(pp.blurb, "Combine PayPal payments with AI.");
        assert!(pp.is_online());
        // 双语名取中文
        assert_eq!(events[1].name, "SpaceXAI 澳门黑客松 2026");
        assert_eq!(events[1].place, "Macau");
        assert_eq!(events[1].mode, "in person");
        assert_eq!(events[1].audience, "for students");
        let item = pp.to_feed_item();
        assert_eq!(item.id, "hack123-paypal-ai-hackathon-2026");
        assert!(item.summary.contains("截止 2026-11-13"));
    }
}
