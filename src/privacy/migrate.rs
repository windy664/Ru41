//! Vault 明文密码迁移：把笔记里的密码搬进凭据托管库，原地换成凭据引用。
//!
//! 与 `scan` 的区别：扫描只报位置（报告可能被推送，绝不带值）；迁移要取值，
//! 因此值只在本进程内存里流动——写进凭据库，不进日志、不进报告、不回显。
//!
//! 迁移后笔记里留的是「凭据「名字」」引用：知识库照常可检索，agent 需要真值时
//! 经 `ru41 secret get <名字>` 自取（真值不进模型上下文）。

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// 凭据记录类别（与 credentials 插件、核心 CLI `ru41 secret` 同一份数据）。
pub const CREDENTIAL_KIND: &str = "credential";

/// 一条待迁移的明文：位置 + 派生名称 + 原值（私有，绝不序列化）。
#[derive(Debug, Clone)]
pub struct PlannedSecret {
    /// 相对 vault 根的路径（正斜杠）。
    pub path: String,
    /// 行号（1 起）。
    pub line: usize,
    /// 命中规则名。
    pub rule: &'static str,
    /// 派生出的凭据名。
    pub name: String,
    /// 明文原值：只用于写入凭据库。
    value: String,
    /// 原值在文件中的字节区间（start..end）。
    span: (usize, usize),
}

/// 迁移计划（对外只暴露位置与名称）。
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub path: String,
    pub line: usize,
    pub rule: String,
    pub name: String,
}

/// 迁移结果。
#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    pub credentials: usize,
    pub rewritten_files: usize,
    pub entries: Vec<PlanEntry>,
}

struct Extract {
    rule: &'static str,
    regex: Regex,
}

/// 取值版规则库：扫描规则的超集，补上「密码是 xxx」「用户名和密码都是 xxx」
/// 这类自然语句（扫描器的 secret_kv 要求键值形态，抓不到它们）。
fn extractors() -> &'static [Extract] {
    static EXTRACTORS: OnceLock<Vec<Extract>> = OnceLock::new();
    EXTRACTORS.get_or_init(|| {
        let make = |rule: &'static str, pattern: &str| Extract {
            rule,
            regex: Regex::new(pattern).expect("extract pattern must compile"),
        };
        vec![
            // 键值形态：password: xxx / 密码：xxx
            make(
                "secret_kv",
                r#"(?i)(?:^|[^0-9A-Za-z_])?(?:password|passwd|pwd|密码|口令|secret|api[_-]?key|apikey|access[_-]?token|refresh[_-]?token|client[_-]?secret|token)\b\W*[:：=]\W*(?:"|')?([!-~]{6,})(?:"|')?"#,
            ),
            // 自然语句：密码是 xxx / 用户名和密码都是 xxx / password is xxx
            // 必须有连接词，否则「密码学」「密码 bucket」这类会误命中。
            make(
                "口令语句",
                r#"(?i)(?:用户名和密码|账号密码|登录密码|初始密码|密码|口令|password|passwd|pwd)(?:是|均为|都是|为|：|:|＝|=|\bis\b)\s*[`'""]?([!-~]{4,64})[`'""]?"#,
            ),
            // 反引号/引号包裹的值：`- **上传**：密码 `upload_80894``
            make(
                "引号口令",
                r#"(?i)(?:用户名和密码|账号密码|登录密码|初始密码|密码|口令|password|passwd|pwd)\s*[`'"]([!-~]{4,64})[`'"]"#,
            ),
            make(
                "bearer_token",
                r"\b[Bb]earer\s+([A-Za-z0-9._~+/=-]{10,})",
            ),
            make(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            ),
            make("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
            make("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
            make("github_token", r"\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{30,}\b"),
            make("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b"),
            // 带凭据的连接串：取密码段（组 2）。
            make(
                "cred_url",
                r"\b(?:postgres(?:ql)?|mysql|redis|mongodb(?:\+srv)?|amqp|ftp)://([^\s/:@]+):([^\s/@]+)@",
            ),
        ]
    })
}

/// 规则的值捕获组序号：0 = 整条匹配即值，其余为该组。
fn value_group(rule: &str) -> usize {
    match rule {
        "jwt" | "aws_access_key" | "slack_token" | "github_token" | "openai_key" => 0,
        "cred_url" => 2,
        _ => 1,
    }
}

/// 扫描 vault，产出迁移计划（值只在内存）。
pub fn plan(root: &Path) -> Result<Plan> {
    Ok(Plan {
        entries: scan(root)?.into_iter().map(PlanEntry::from).collect(),
    })
}

/// 执行迁移：写凭据库 + 把笔记里的明文替换成凭据引用。
pub fn apply(
    root: &Path,
    store: &dyn crate::ports::storage::TaskStore,
    now: i64,
) -> Result<Applied> {
    let mut entries = Vec::new();
    let mut credentials = 0usize;
    let mut files = Vec::new();
    for secret in scan(root)? {
        let revision = store
            .document(CREDENTIAL_KIND, &secret.name)?
            .map(|doc| doc.revision);
        let written = store.put_document(
            CREDENTIAL_KIND,
            &secret.name,
            revision,
            &serde_json::json!({"value": secret.value, "updated_at": now}),
        )?;
        if !written {
            bail!("凭据写入冲突：{}", secret.name);
        }
        credentials += 1;
        rewrite_file(root, &secret)?;
        if !files.contains(&secret.path) {
            files.push(secret.path.clone());
        }
        entries.push(PlanEntry::from(secret));
    }
    Ok(Applied {
        credentials,
        rewritten_files: files.len(),
        entries,
    })
}

impl From<PlannedSecret> for PlanEntry {
    fn from(secret: PlannedSecret) -> Self {
        PlanEntry {
            path: secret.path,
            line: secret.line,
            rule: secret.rule.to_owned(),
            name: secret.name,
        }
    }
}

/// 扫描并保留明文（apply 用；plan 走同一函数）。
fn scan(root: &Path) -> Result<Vec<PlannedSecret>> {
    let mut files = Vec::new();
    collect_text_files(root, &mut files)?;
    files.sort();
    let mut out = Vec::new();
    let mut used_names: Vec<String> = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "凭据".to_owned());
        let mut seen_spans: Vec<(usize, usize)> = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let line_start = text
                .split_inclusive('\n')
                .take(idx)
                .map(str::len)
                .sum::<usize>();
            for extract in extractors() {
                let group = value_group(extract.rule);
                for caps in extract.regex.captures_iter(line) {
                    let Some(value_match) = caps.get(group) else {
                        continue;
                    };
                    let value = value_match.as_str().to_owned();
                    if value.is_empty() {
                        continue;
                    }
                    let span = (
                        line_start + value_match.start(),
                        line_start + value_match.end(),
                    );
                    // 同一段文本被多条规则命中时只取一条。
                    if seen_spans.contains(&span) {
                        continue;
                    }
                    seen_spans.push(span);
                    let name = derive_name(line, &value, &stem, &used_names);
                    used_names.push(name.clone());
                    out.push(PlannedSecret {
                        path: rel.clone(),
                        line: idx + 1,
                        rule: extract.rule,
                        name,
                        value,
                        span,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// 把一行里的明文替换成 `凭据「名字」`。
fn rewrite_file(root: &Path, secret: &PlannedSecret) -> Result<()> {
    let path = root.join(&secret.path);
    let mut text =
        std::fs::read_to_string(&path).with_context(|| format!("读 {}", path.display()))?;
    if secret.span.1 > text.len() {
        return Ok(()); // 文件已被其他写者改动，跳过这一条。
    }
    text.replace_range(
        secret.span.0..secret.span.1,
        &format!("凭据「{}」", secret.name),
    );
    std::fs::write(&path, text).with_context(|| format!("写 {}", path.display()))?;
    Ok(())
}

/// 从上下文派生凭据名：优先取行内的 `**标签**`，否则取关键词前的一小段文字。
fn derive_name(line: &str, value: &str, stem: &str, used: &[String]) -> String {
    let mut label = String::new();
    // 行内第一个粗体标签：「- **上传作业**：…」→「上传作业」。
    if let Some(start) = line.find("**")
        && let Some(end) = line[start + 2..].find("**")
    {
        label = line[start + 2..start + 2 + end].to_owned();
    }
    if label.is_empty() {
        // 退而求其次：值之前的文字，取末尾一段并去掉标点。
        if let Some(pos) = line.find(value) {
            let cleaned = line[..pos]
                .trim_end_matches(|c: char| {
                    c.is_whitespace() || "：:（(【[]「」\"'`*—-,、是均为".contains(c)
                })
                .to_owned();
            label = cleaned
                .chars()
                .rev()
                .take(12)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
                .trim()
                .to_owned();
            // 名字里不必重复「密码」这类词：「ftp 密码是 xxx」→「ftp」。
            for word in ["密码", "口令", "password", "passwd"] {
                if let Some(stripped) = label.strip_suffix(word) {
                    label = stripped.trim().to_owned();
                    break;
                }
            }
        }
    }
    if label.is_empty() {
        label = stem.to_owned();
    }
    let mut name: String = label
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '*' | '`' | '"' | '\'' | '|' | '>' | '<'))
        .take(24)
        .collect();
    if name.is_empty() {
        name = "凭据".to_owned();
    }
    // 同名去重：加序号。
    let mut candidate = name.clone();
    let mut index = 2;
    while used.iter().any(|other| other == &candidate) {
        candidate = format!("{name}-{index}");
        index += 1;
    }
    candidate
}

fn collect_text_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_label_from_bold_and_dedupes() {
        assert_eq!(
            derive_name(
                "- **上传作业**：`ftp://172.16.3.242`（用户名和密码都是 upload_80894）",
                "upload_80894",
                "note",
                &[]
            ),
            "上传作业"
        );
        let used = vec!["ftp".to_owned()];
        assert_eq!(
            derive_name("ftp 密码是 abc123", "abc123", "note", &used),
            "ftp-2"
        );
    }

    #[test]
    fn extracts_natural_sentence_passwords() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "- **上传作业**：`ftp://172.16.3.242`（用户名和密码都是 upload_80894）\n- 正常行 token bucket\n",
        )
        .unwrap();
        let plan = plan(&vault).unwrap();
        assert_eq!(plan.entries.len(), 1, "{plan:?}");
        assert_eq!(plan.entries[0].name, "上传作业");
    }

    #[test]
    fn questions_and_lookalikes_are_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "我的密码是多少来着\n密码 6 位\n密码学原理\n密码 bucket 机制\n",
        )
        .unwrap();
        assert!(plan(&vault).unwrap().entries.is_empty());
    }
}
