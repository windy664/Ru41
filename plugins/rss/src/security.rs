//! 必报硬闸：**代码层**判定高危安全条目，不靠 LLM 现场判断。
//!
//! 为什么要有这个(2026-11-21 实测): 早报里最该报的东西会被 LLM 丢掉 ——
//! Java4Shell（OpenJDK Base64 解码器 RCE，CVSS 9.2，无需认证即可远程接管）
//! 被写进了「已舍弃：…安全漏洞细节…」那一行。原因是 prompt 只说了「筛选值得关注的」，
//! 没给**必须报**的硬判据，LLM 每天自己现场判，判一次丢一次。
//!
//! 这里的做法：判据写在**代码**里(强信号 + 编号 + 空洞检查)，命中的条目
//! **单独成一条 digest 任务**（prompt 里只有这些条目、且明令一条都不许舍弃），
//! 所以「必报」不再依赖 LLM 的判断力。
//!
//! 判据（与用户口径一致）：
//! - 强信号：CVSS ≥ 9.0 / Critical / RCE / 0day / 无需认证 / 已在野利用；
//! - 弱信号：只有 CVE 编号 —— 仅当条目来自**权威漏洞源**（GHSA / CISA KEV /
//!   NVD / php.net）时才算必报，否则只是正文里提了一嘴；
//! - 空洞公告不成条：没有编号、没有评分、没有版本号、也读不出漏洞语境的
//!   条目（典型如「XX 发布安全公告」）**不报** —— 报了等于占位。

use regex::Regex;
use std::sync::LazyLock;

/// CVSS 分值：`CVSS 9.8` / `CVSS 3.1 score 9.8` / `CVSS:9.8`。
/// 向量形式 `CVSS:3.1/AV:N/...` 里的 3.1 是版本号不是分值，由 extract_cvss 排除。
static CVSS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bcvss(?:\s*(?:base|vector))?\s*(?:score)?\s*[:：]?\s*(\d{1,2}(?:\.\d+)?)")
        .expect("cvss regex")
});

/// `score 9.8` 形式（`CVSS:3.1/AV:N/... score 9.8`）—— 分值跟在 "score" 后面，
/// 这时前面那个 3.1 是向量版本号。
static CVSS_SCORE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bscore\b[^0-9]{0,10}(\d{1,2}(?:\.\d+)?)").expect("cvss score regex")
});

/// CVE / GHSA 编号。
static CVE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:CVE-\d{4}-\d{4,7}|GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4})\b").expect("cve regex"));

/// 版本号样式的数字（8.6.0 / 3.0.12 / 8.5.13）。
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d+\.\d+(?:\.\d+)*\b").expect("version regex"));

/// 漏洞语境：说清了「这是个漏洞」的词。
static VULN_CONTEXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        // ⚠️ 「安全公告/advisory」**故意不在这里**：那正是空洞公告的标志词
        // (「XX 发布安全公告」没有任何实质信息)，当事实会让空洞判据失效。
        r"(?i)漏洞|vulnerab|利用|exploit|注入|injection|越权|接管|远程代码执行|远程命令执行|RCE|0-?day|零日|在野",
    )
    .expect("vuln context regex")
});

/// Critical 严重等级。
static CRITICAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bcritical\b|严重等级\s*critical").expect("critical regex"));

/// 源给出的严重等级：「严重等级 high」这种（安全源适配器写进摘要里的）。
static SEVERITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)严重等级\s*(low|moderate|medium|high|critical)").expect("severity regex"));

/// 远程代码执行。
static RCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bRCE\b|远程代码执行|远程命令执行").expect("rce regex"));

/// 0day / 零日。
static ZERO_DAY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b0-?day\b|零日").expect("zero day regex"));

/// 无需认证即可利用。
static NO_AUTH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)无需认证|无需鉴权|无需登录|无需授权|未授权(?:即可)?(?:访问|利用|远程)|unauthenticated|pre-auth|without authentication",
    )
    .expect("no auth regex")
});

/// 已在野利用。
static IN_WILD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)在野利用|已被利用|已被攻击|已知被利用|已有真实攻击|actively exploited|exploited in the wild|known exploited|已知被利用漏洞",
    )
    .expect("in wild regex")
});

/// 必报门槛：CVSS 多少算必报。
pub const MUST_REPORT_CVSS: f64 = 9.0;

/// 条目来源类别 —— 决定「只带 CVE 编号」这种**弱信号**算不算必报。
///
/// 为什么要有这个(2026-11-22 实测): 弱信号规则一放开，GHSA 一天 100 条里
/// 45 条是 contao 之类 CMS 的中低危 XSS —— 全进必报段，等于没有必报段。
/// 分级之后：官方漏洞公告源要**高危/严重**才算，KEV（已被利用）与
/// 语言官方安全发布直接算。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 普通资讯源（正文里提了一嘴 CVE 不算）。
    News,
    /// 官方漏洞公告源（GHSA / NVD）—— 要高危/严重才算。
    Advisory,
    /// 已在野利用目录（CISA KEV）—— 本身就是「已被利用」，算。
    KEV,
    /// 语言/运行时官方安全发布（php.net）—— 算。
    Release,
}

impl Origin {
    /// 从条目 id 前缀判断来源（安全源适配器给自己的 id 加了固定前缀）。
    pub fn of_item(id: &str) -> Origin {
        if id.starts_with("kev-") {
            Origin::KEV
        } else if id.starts_with("php-") {
            Origin::Release
        } else if id.starts_with("ghsa-") || id.starts_with("nvd-") {
            Origin::Advisory
        } else {
            Origin::News
        }
    }
}

/// 判定结果。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Verdict {
    /// 命中的强信号标签（必报理由），直接进 prompt 让人一眼看懂为什么必报。
    pub reasons: Vec<String>,
    /// 材料里读到的 CVSS 分值。
    pub cvss: Option<f64>,
    /// 是否带 CVE / GHSA 编号。
    pub has_cve_id: bool,
    /// 源给出的严重等级（low/moderate/high/critical）；读不到为 None。
    pub severity: Option<String>,
    /// 是否有事实性细节（编号/评分/版本号/漏洞语境）—— 反过来就是「空洞公告」。
    pub has_fact: bool,
}

impl Verdict {
    /// 是否命中强信号。
    pub fn is_strong(&self) -> bool {
        !self.reasons.is_empty()
    }
}

/// 从材料（标题 + 摘要）里判定。
pub fn verdict(title: &str, summary: &str) -> Verdict {
    let text = format!("{title} {summary}");
    let mut v = Verdict::default();

    v.cvss = extract_cvss(&text);
    v.has_cve_id = CVE_RE.is_match(&text);
    v.severity = SEVERITY_RE
        .captures(&text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_ascii_lowercase());
    let has_version = VERSION_RE.is_match(&text);
    let has_context = VULN_CONTEXT_RE.is_match(&text);
    v.has_fact = v.has_cve_id || v.cvss.is_some() || has_version || has_context;

    if let Some(score) = v.cvss
        && score >= MUST_REPORT_CVSS
    {
        v.reasons.push(format!("CVSS {score}"));
    }
    if CRITICAL_RE.is_match(&text) {
        v.reasons.push("严重等级 Critical".to_owned());
    }
    if RCE_RE.is_match(&text) {
        v.reasons.push("RCE（远程代码执行）".to_owned());
    }
    if ZERO_DAY_RE.is_match(&text) {
        v.reasons.push("0day".to_owned());
    }
    if NO_AUTH_RE.is_match(&text) {
        v.reasons.push("无需认证即可利用".to_owned());
    }
    if IN_WILD_RE.is_match(&text) {
        v.reasons.push("已在野利用".to_owned());
    }
    v
}

/// 从材料里抠 CVSS 分值；向量里的版本号（3.1/4.0）不算分值。
fn extract_cvss(text: &str) -> Option<f64> {
    // 先看 "score X" —— 有它时前面的 "CVSS:3.1" 一定���向量版本号。
    if let Some(caps) = CVSS_SCORE_RE.captures(text)
        && let Some(num) = caps.get(1)
        && let Ok(value) = num.as_str().parse::<f64>()
    {
        return Some(value);
    }
    for caps in CVSS_RE.captures_iter(text) {
        let Some(num) = caps.get(1) else {
            continue;
        };
        let Ok(value) = num.as_str().parse::<f64>() else {
            continue;
        };
        let after = &text[num.end()..];
        // `CVSS:3.1/AV:N/...` —— 后面跟 '/' 的是向量，版本号不是分值。
        if after.trim_start().starts_with('/') {
            continue;
        }
        // 版本号形态（2.0/3.0/3.1/4.0）也不当分值。
        if [2.0_f64, 3.0, 3.1, 4.0]
            .iter()
            .any(|v| (value - v).abs() < f64::EPSILON)
        {
            continue;
        }
        return Some(value);
    }
    None
}

/// 这条要不要进必报。
///
/// - 强信号（CVSS ≥ 9.0 / Critical / RCE / 0day / 无需认证 / 已在野利用）→ 必报；
/// - 弱信号（只带 CVE 编号）→ 看来源：
///   - KEV（已在野利用目录）与语言官方安全发布 → 必报；
///   - 官方漏洞公告源（GHSA / NVD）→ 要**高危/严重**才算（否则一天 45 条 XSS 全进必报段）；
///   - 普通资讯源 → 不算（正文里提一嘴 CVE 而已）。
pub fn must_report(v: &Verdict, origin: Origin) -> bool {
    // 空洞公告不成条：没有实质信息的不报（报了只是占位）。
    if !v.has_fact {
        return false;
    }
    if v.is_strong() {
        return true;
    }
    if !v.has_cve_id {
        return false;
    }
    match origin {
        Origin::KEV | Origin::Release => true,
        Origin::Advisory => matches!(v.severity.as_deref(), Some("high") | Some("critical")),
        Origin::News => false,
    }
}

/// 便利入口：直接判定一条 FeedItem 是否必报。
pub fn is_must_report(title: &str, summary: &str, origin: Origin) -> bool {
    must_report(&verdict(title, summary), origin)
}

/// 必报理由的一行话（进 prompt）。
pub fn reason_line(v: &Verdict) -> String {
    if v.reasons.is_empty() {
        return "官方漏洞源条目".to_owned();
    }
    v.reasons.join("、")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvss_high_score_is_strong() {
        let v = verdict(
            "OpenJDK Base64 解码器远程代码执行漏洞",
            "CVSS 9.2，无需认证即可远程接管",
        );
        assert!(v.is_strong(), "{v:?}");
        assert!(v.reasons.iter().any(|r| r.contains("CVSS 9.2")), "{v:?}");
        assert!(v.reasons.iter().any(|r| r.contains("无需认证")), "{v:?}");
        assert!(must_report(&v, Origin::News));
    }

    #[test]
    fn cvss_vector_version_is_not_a_score() {
        // "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:H" 里的 3.1 是版本号
        let v = verdict("某库漏洞公告", "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:L/A:H");
        assert_eq!(v.cvss, None, "{v:?}");
        // 真实分值仍能读出来
        let v2 = verdict(
            "某库漏洞公告 CVE-2026-12345",
            "严重等级 high · CVSS 3.1 score 8.2",
        );
        assert_eq!(v2.cvss, Some(8.2), "{v2:?}");
        // 8.2 不够 9.0 → 不是强信号，但有编号 → 官方源里仍必报
        assert!(!v2.is_strong(), "{v2:?}");
        assert!(must_report(&v2, Origin::Advisory));
        assert!(!must_report(&v2, Origin::News));
    }

    #[test]
    fn rce_zero_day_and_in_wild_are_strong() {
        for (title, summary) in [
            ("某框架曝 RCE 漏洞", "远程代码执行，可被利用"),
            ("某浏览器 0day 已被利用", "攻击者已在野利用"),
            ("某设备无需认证即可远程接管", "无需认证，CVSS 未标注"),
        ] {
            let v = verdict(title, summary);
            assert!(v.is_strong(), "{title} → {v:?}");
            assert!(must_report(&v, Origin::News), "{title}");
        }
    }

    #[test]
    fn hollow_announcement_is_not_reported() {
        // 只有「发布安全公告」四个字，没有任何实质信息 → 空洞，不成条
        let v = verdict("某公司发布年度安全公告", "更多细节请关注官方渠道。");
        assert!(!v.has_fact, "{v:?}");
        assert!(!must_report(&v, Origin::KEV));
        assert!(!must_report(&v, Origin::News));
    }

    #[test]
    fn plain_cve_mention_from_news_is_not_must_report() {
        let v = verdict("某项目修复了一个漏洞", "详见 CVE-2026-12345");
        assert!(v.has_cve_id);
        assert!(!v.is_strong(), "{v:?}");
        assert!(!must_report(&v, Origin::News), "非权威源只提一嘴 CVE 不必报");
        assert!(must_report(&v, Origin::KEV), "权威漏洞源里的 CVE 条目必报");
    }

    #[test]
    fn php_security_release_is_must_report() {
        let v = verdict(
            "PHP 8.5.13 安全发布：3 个安全修复（含 GHSA/CVE 编号）",
            "PHP 8.5.13 安全发布 · 受影响版本提示：All PHP 8.5 users · GHSA-8mjj-2r7p-4hg2 (Use-after-free in magic method backtraces can lead to remote code execution). (CVE-2026-129474)",
        );
        assert!(v.has_cve_id, "{v:?}");
        assert!(v.has_fact);
        assert!(must_report(&v, Origin::KEV));
    }

    #[test]
    fn kev_entry_is_must_report_by_in_wild() {
        let v = verdict(
            "CVE-2026-99999：Acme Widget RCE",
            "CISA 已知被利用漏洞目录（KEV）· 受影响：Acme Widget · Acme widgets allow remote code execution without authentication. · 处置期限 2026-12-01",
        );
        assert!(v.reasons.iter().any(|r| r.contains("已在野")), "{v:?}");
        assert!(v.reasons.iter().any(|r| r.contains("无需认证")), "{v:?}");
        assert!(must_report(&v, Origin::News));
    }

    /// 官方漏洞公告源里，**中低危**的条目不算必报（GHSA 一天几十条 XSS）。
    #[test]
    fn advisory_source_needs_high_or_critical_severity() {
        // GHSA 的 moderate 级别漏洞（有 CVE 编号，但严重度不够）
        let v = verdict(
            "GHSA-xxxx-yyyy-zzzz：某 CMS 的 XSS",
            "严重等级 moderate · 官方公告，详见 CVE-2026-12345。",
        );
        assert!(v.has_cve_id);
        assert_eq!(v.severity.as_deref(), Some("moderate"));
        assert!(!must_report(&v, Origin::Advisory), "中危不算必报");
        // 同一来源，high 级别就算
        let v2 = verdict(
            "GHSA-xxxx-yyyy-zzzz：某 CMS 的权限提升",
            "严重等级 high · 官方公告，详见 CVE-2026-12345。",
        );
        assert!(must_report(&v2, Origin::Advisory), "高危要算必报");
        // 普通资讯源提到同一个 CVE 仍然不算
        assert!(!must_report(&v2, Origin::News));
    }

    /// KEV（已被利用目录）与语言官方安全发布：只带 CVE 编号也算必报。
    #[test]
    fn kev_and_release_sources_count_cve_only_items() {
        let v = verdict("CVE-2026-99999：某设备漏洞", "官方目录条目，详见 CVE-2026-99999。");
        assert!(must_report(&v, Origin::KEV));
        assert!(must_report(&v, Origin::Release));
        assert!(!must_report(&v, Origin::News));
    }

    /// 条目 id 前缀 → 来源类别。
    #[test]
    fn origin_from_item_id_prefix() {
        assert_eq!(Origin::of_item("ghsa-GHSA-x"), Origin::Advisory);
        assert_eq!(Origin::of_item("nvd-CVE-2026-1"), Origin::Advisory);
        assert_eq!(Origin::of_item("kev-CVE-2026-1"), Origin::KEV);
        assert_eq!(Origin::of_item("php-8.5.13"), Origin::Release);
        assert_eq!(Origin::of_item("https://news.ycombinator.com/rss#1"), Origin::News);
    }

    #[test]
    fn ordinary_news_is_not_must_report() {
        let v = verdict("某公司发布开源模型", "免费额度，可自行部署");
        assert!(!v.is_strong());
        assert!(!must_report(&v, Origin::News));
        assert!(!must_report(&v, Origin::KEV));
    }
}
