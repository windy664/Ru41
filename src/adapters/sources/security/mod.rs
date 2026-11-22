//! 安全漏洞源适配器：GHSA / CISA KEV / NVD / php.net。
//!
//! 为什么要有这些源(2026-11-21): 早报里最该报的东西(0day / RCE / 已在野利用)
//! **压根没进候选池** —— 当时订阅的都是资讯站(机器之心/IT之家/…)，
//! 漏洞公告要么不进这些源、要么是二手转述没有 CVSS 与编号。
//! 结果是「PHP 8.6 修了 6 个 Critical RCE」这类信息**根本进不来**。
//!
//! 四个源各管一段：
//! - `ghsa+`：GitHub 官方 Advisory Database（REST API，免认证）—— 有 GHSA 编号、
//!   CVE 编号、CVSS 分值与向量、官方描述。是结构化漏洞源里最干净的一个。
//! - `kev+`：CISA 已知被利用漏洞目录（KEV，JSON）—— **已在野利用**，
//!   带强制处置期限(dueDate)与勒索软件标记。这是「现在就修」的那一档。
//! - `nvd+`：NVD CVE API 2.0 —— 只取 CRITICAL（全量每天上千条，会淹掉日报）。
//! - `php+`：php.net 官方发布 Atom + ChangeLog —— PHP 的安全修复在
//!   ChangeLog 里带 GHSA-/CVE- 编号（Atom 条目本身只有「有新版本了」，是空洞公告）。
//!
//! 这些源的条目会被 plugins/rss 的**必报硬闸**（plugins/rss/src/security.rs）挑出来，
//! 单独成段必报，不与普通资讯抢注意力。

pub mod ghsa;
pub mod kev;
pub mod nvd;
pub mod php;

/// 权威漏洞源的 feed 地址前缀集合（plugins/rss 用它判断「这条是不是官方漏洞公告」）。
///
/// 前缀而不是完整 URL：配置里写 `ghsa+https://api.github.com/advisories?...`，
/// 前缀足够识别来源。
pub const AUTHORITATIVE_PREFIXES: &[&str] = &[
    ghsa::URL_PREFIX,
    kev::URL_PREFIX,
    nvd::URL_PREFIX,
    php::URL_PREFIX,
];

/// 把 URL 里的自定义 `days=` 参数摘出来：(去掉 days 的 URL, 天数)。
///
/// 摘的时候顺手去掉紧挨着 days 的分隔符（? 或 &），否则会留下悬空的
/// 「...?a=1&」这种畸形 URL。
pub fn split_days(url: &str, default_days: u32) -> (String, u32) {
    let Some(idx) = url.find("days=") else {
        return (url.to_owned(), default_days);
    };
    let value: String = url[idx + 5..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let days = value.parse::<u32>().unwrap_or(default_days);
    let end = idx + 5 + value.len();
    let mut base = url[..idx].to_owned();
    if base.ends_with('?') || base.ends_with('&') {
        base.pop();
    }
    base.push_str(&url[end..]);
    (base, days)
}

/// 权威漏洞源在**条目 id** 上的前缀（与 URL 前缀对应）。
///
/// 为什么看 id 而不是 URL：候选池里只存 FeedItem，源 URL 在入池时就被丢掉了；
/// 而各安全源适配器都把自己的 id 加了固定前缀（ghsa- / kev- / nvd- / php-），
/// 所以从 id 就能认出条目来自哪个权威源。
pub const AUTHORITATIVE_ID_PREFIXES: &[&str] = &["ghsa-", "kev-", "nvd-", "php-"];

/// 条目是否来自权威漏洞源（GHSA / CISA KEV / NVD / php.net）。
pub fn is_authoritative_item(id: &str) -> bool {
    AUTHORITATIVE_ID_PREFIXES
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

/// feed 地址是否是权威漏洞源（GHSA / KEV / NVD / php.net）。
pub fn is_authoritative_feed(feed_url: &str) -> bool {
    AUTHORITATIVE_PREFIXES
        .iter()
        .any(|prefix| feed_url.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_authoritative_feeds() {
        assert!(is_authoritative_feed(
            "ghsa+https://api.github.com/advisories?per_page=100"
        ));
        assert!(is_authoritative_feed(
            "kev+https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json"
        ));
        assert!(is_authoritative_feed(
            "nvd+https://services.nvd.nist.gov/rest/json/cves/2.0"
        ));
        assert!(is_authoritative_feed(
            "php+https://www.php.net/releases/feed.php"
        ));
        // 普通资讯源不是漏洞源
        assert!(!is_authoritative_feed("https://news.ycombinator.com/rss"));
        assert!(!is_authoritative_feed("https://www.ithome.com/rss/"));
        assert!(!is_authoritative_feed(""));
    }

    #[test]
    fn split_days_strips_the_param_cleanly() {
        // ?days=7
        let (base, days) = super::split_days("https://x/j.json?days=7", 3);
        assert_eq!(base, "https://x/j.json");
        assert_eq!(days, 7);
        // ?a=1&days=7 —— 不能留下悬空的 &
        let (base, days) = super::split_days("https://x/a?b=1&days=5", 3);
        assert_eq!(base, "https://x/a?b=1");
        assert_eq!(days, 5);
        // 没有 days → 原样返回 + 默认值
        let (base, days) = super::split_days("https://x/a?b=1", 3);
        assert_eq!(base, "https://x/a?b=1");
        assert_eq!(days, 3);
    }

    #[test]
    fn recognizes_authoritative_items_by_id_prefix() {
        assert!(is_authoritative_item("ghsa-GHSA-25qc-m4pw-2w8r"));
        assert!(is_authoritative_item("kev-CVE-2026-99999"));
        assert!(is_authoritative_item("nvd-CVE-2026-11111"));
        assert!(is_authoritative_item("php-8.5.13"));
        assert!(!is_authoritative_item(
            "https://news.ycombinator.com/rss#12345"
        ));
        assert!(!is_authoritative_item(""));
    }
}
