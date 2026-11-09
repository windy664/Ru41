//! Vault 明文密钥扫描：找出笔记里疑似密码/密钥/令牌的行。
//! 报告只含位置、规则名和脱敏预览——报告本身可能被推送或归档，绝不携带原值。

use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// 单行命中：行号 + 命中规则 + 已脱敏的行预览。
#[derive(Debug, Clone, Serialize)]
pub struct SecretHit {
    pub line: usize,
    pub rule: &'static str,
    /// 命中行原文，敏感片段替换为 `<规则名>`；截断到 160 字符。
    pub preview: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub path: String,
    pub hits: Vec<SecretHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub scanned_files: usize,
    pub flagged_files: usize,
    pub total_hits: usize,
    pub files: Vec<FileReport>,
}

struct Pattern {
    rule: &'static str,
    regex: regex::Regex,
    /// 敏感值所在捕获组；捕获到的值是掩码（********）时不算命中。
    value_group: usize,
}

/// 扫描用的形态库：脱敏规则（secret_kv/bearer/jwt）的父集 + 常见厂商密钥特征。
/// 与 RandomSanitizer 的规则是两套用途：脱敏要在发送前替换，扫描只在本地报警。
fn patterns() -> &'static [Pattern] {
    static PATTERNS: OnceLock<Vec<Pattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let make = |rule: &'static str, pattern: &str, value_group: usize| Pattern {
            rule,
            regex: regex::Regex::new(pattern).expect("scan pattern must compile"),
            value_group,
        };
        vec![
            make(
                "secret_kv",
                r#"(?i)(?:^|[^0-9A-Za-z_])?(?:password|passwd|pwd|密码|口令|secret|api[_-]?key|apikey|access[_-]?token|refresh[_-]?token|client[_-]?secret|token)\b\W*[:：=]\W*(?:"|')?([!-~]{6,})(?:"|')?"#,
                1,
            ),
            make("bearer_token", r"\b[Bb]earer\s+([A-Za-z0-9._~+/=-]{10,})", 1),
            make(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
                0,
            ),
            make("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b", 0),
            make("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}", 0),
            make("github_token", r"\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{30,}\b", 0),
            make("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b", 0),
            make(
                "private_key",
                r"-----BEGIN (?:RSA |EC |OPENSSH |PGP |DSA )?PRIVATE KEY-----",
                0,
            ),
            make(
                "cred_url",
                r"\b(?:postgres(?:ql)?|mysql|redis|mongodb(?:\+srv)?|amqp)://[^\s/:@]+:([^\s/@]+)@",
                1,
            ),
        ]
    })
}

/// 扫一段文本，返回逐行命中（每行可能命中多条规则）。
pub fn scan_text(text: &str) -> Vec<SecretHit> {
    let mut hits = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let mut masked = line.to_owned();
        for pattern in patterns() {
            // 掩码值（********、your_password）不是真凭据，不报警。
            let hit = pattern.regex.captures_iter(line).any(|caps| {
                caps.get(pattern.value_group)
                    .map(|value| !crate::privacy::is_placeholder(value.as_str()))
                    .unwrap_or(false)
            });
            if hit {
                masked = pattern
                    .regex
                    .replace_all(&masked, format!("<{}>", pattern.rule))
                    .into_owned();
                hits.push(SecretHit {
                    line: idx + 1,
                    rule: pattern.rule,
                    preview: truncate(&masked, 160),
                });
            }
        }
    }
    hits
}

/// 递归扫描 vault 目录下的 md/txt/canvas 文件；跳过 `.git`、`.obsidian`、`.trash`
/// 等点开头的目录与明显的二进制附件。路径为相对 root 的正斜杠形式。
/// `excludes` 为路径子串排除名单（博客示例、归档项目文档等不算残留）。
pub fn scan_vault(root: &Path, excludes: &[String]) -> std::io::Result<ScanReport> {
    let mut files = Vec::new();
    collect_text_files(root, &mut files)?;
    files.sort();
    files.retain(|path| {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        !excludes.iter().any(|needle| rel.contains(needle.as_str()))
    });
    let mut report = ScanReport {
        scanned_files: files.len(),
        flagged_files: 0,
        total_hits: 0,
        files: Vec::new(),
    };
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue; // 非文本（二进制附件）直接跳过。
        };
        let hits = scan_text(&text);
        if hits.is_empty() {
            continue;
        }
        report.flagged_files += 1;
        report.total_hits += hits.len();
        report.files.push(FileReport {
            path: path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/"),
            hits,
        });
    }
    Ok(report)
}

fn collect_text_files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            // 跳过 VCS 元数据、Obsidian 配置、回收站等非笔记目录。
            if name.starts_with('.') {
                continue;
            }
            collect_text_files(&path, out)?;
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md") | Some("txt") | Some("canvas")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max_chars).collect();
    cut.push('…');
    cut
}

/// 红区替换：把密钥形态片段替换为 `<规则名>` 占位符，键名保留。
/// 与 sanitize 不同：无映射、无回填——用于正文快照进 prompt（原文由宿主持有，
/// Agent 侧只见占位符）。 secret_kv 类命中只换值，LLM 仍知道字段语义。
pub fn redact_secrets(text: &str) -> String {
    let mut result = text.to_owned();
    for pattern in patterns() {
        let has_value_group = pattern.regex.captures_len() > 1;
        result = pattern
            .regex
            .replace_all(&result, |captures: &regex::Captures| {
                if has_value_group {
                    let full = captures.get(0).unwrap();
                    let value = captures.get(1).unwrap();
                    format!(
                        "{}<{}>",
                        &full.as_str()[..value.start() - full.start()],
                        pattern.rule
                    )
                } else {
                    format!("<{}>", pattern.rule)
                }
            })
            .into_owned();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_common_secret_shapes_with_redacted_preview() {
        let text = "我的密码: Sup3rSecret99\n正常笔记行\nAKIAIOSFODNN7EXAMPLE 是示例\n";
        let hits = scan_text(text);
        let rules: Vec<_> = hits.iter().map(|h| h.rule).collect();
        assert!(rules.contains(&"secret_kv"), "rules: {rules:?}");
        assert!(rules.contains(&"aws_access_key"));
        assert!(hits.iter().all(|h| !h.preview.contains("Sup3rSecret99")));
    }

    #[test]
    fn masked_values_are_not_flagged() {
        // 掩码值不是真凭据：迁移不搬、扫描不报警（否则门槛永远归不了零）。
        let text = "mySQLPassword: \"********\"\n密码: your_password_here\n";
        assert!(scan_text(text).is_empty(), "{:?}", scan_text(text));
        // 真密码仍然报警。
        assert!(!scan_text("密码: hunter2secret\n").is_empty());
    }

    #[test]
    fn plain_text_is_not_flagged() {
        let text = "# 学习计划\n\n- token bucket 算法\n- 学号 2640243101\n";
        assert!(scan_text(text).is_empty());
    }

    #[test]
    fn redact_secrets_keeps_keys_and_plain_text() {
        let text = "password=hunter2secret\n正常行 token bucket\nBearer sk-live-abcdefghij123456\n";
        let redacted = redact_secrets(text);
        assert!(!redacted.contains("hunter2secret"));
        assert!(redacted.contains("password=<secret_kv>"));
        assert!(redacted.contains("Bearer <bearer_token>"));
        assert!(redacted.contains("正常行 token bucket"));
    }

    #[test]
    fn vault_scan_reports_relative_paths_and_skips_dot_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(vault.join(".git")).unwrap();
        std::fs::create_dir_all(vault.join("notes")).unwrap();
        std::fs::write(vault.join("notes/a.md"), "数据库 password=hunter2secret\n").unwrap();
        std::fs::write(vault.join(".git/leak.md"), "password=shouldskip123\n").unwrap();
        std::fs::write(vault.join("image.png"), b"\x89PNG").unwrap();
        let report = scan_vault(&vault, &[]).unwrap();
        assert_eq!(report.scanned_files, 1);
        assert_eq!(report.flagged_files, 1);
        assert_eq!(report.files[0].path, "notes/a.md");
        assert_eq!(report.files[0].hits[0].rule, "secret_kv");
    }
}
