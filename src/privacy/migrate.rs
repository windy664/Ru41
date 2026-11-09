//! Vault 明文密码迁移：把笔记里的密码搬进凭据托管库，原地换成凭据引用。
//!
//! 与 `scan` 的区别：扫描只报位置（报告可能被推送，绝不带值）；迁移要取值，
//! 因此值只在本进程内存里流动——写进凭据库，不进日志、不进报告、不回显。
//!
//! 迁移后笔记里留的是「凭据「名字」」引用：知识库照常可检索，agent 需要真值时
//! 经 `ru41 secret get <名字>` 自取（真值不进模型上下文）。

use crate::privacy::is_placeholder;
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
    /// 凭据唯一标识（`cred_xxxxxxxxxxxx`）——引用与寻址都用它。
    pub id: String,
    /// 描述性标签（给人看，不参与寻址）。
    pub label: String,
    /// 明文原值：只用于写入凭据库。
    value: String,
    /// 原值在文件中的字节区间（start..end）；含包裹的引号。
    span: (usize, usize),
    /// 纯原值的字节区间（不含引号），用于替换前二次确认。
    value_span: (usize, usize),
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
    /// 凭据唯一标识。
    pub id: String,
    /// 描述性标签（给人认）。
    pub label: String,
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
    /// 值所在捕获组。
    value_group: usize,
    /// 键名所在捕获组（键值形态才有；用来派生凭据名）。
    key_group: Option<usize>,
}

/// 取值版规则库：扫描规则的超集，补上「密码是 xxx」「用户名和密码都是 xxx」
/// 这类自然语句（扫描器的 secret_kv 要求键值形态，抓不到它们）。
fn extractors() -> &'static [Extract] {
    static EXTRACTORS: OnceLock<Vec<Extract>> = OnceLock::new();
    EXTRACTORS.get_or_init(|| {
        let make = |rule: &'static str, pattern: &str, value_group: usize, key_group| Extract {
            rule,
            regex: Regex::new(pattern).expect("extract pattern must compile"),
            value_group,
            key_group,
        };
        vec![
            // 键值形态：password: xxx / 密码：xxx（组1=键，组2=值）
            make(
                "secret_kv",
                r#"(?i)(?:^|[^0-9A-Za-z_])?((?:password|passwd|pwd|密码|口令|secret|api[_-]?key|apikey|access[_-]?token|refresh[_-]?token|client[_-]?secret|token))\b\W*[:：=]\W*(?:"|')?([!-~]{6,})(?:"|')?"#,
                2,
                Some(1),
            ),
            // 自然语句：密码是 xxx / 用户名和密码都是 xxx / password is xxx
            // 必须有连接词，否则「密码学」「密码 bucket」这类会误命中。
            make(
                "口令语句",
                r#"(?i)(?:用户名和密码|账号密码|登录密码|初始密码|密码|口令|password|passwd|pwd)(?:是|均为|都是|为|：|:|＝|=|\bis\b)\s*[`'""]?([!-~]{4,64})[`'""]?"#,
                1,
                None,
            ),
            // 反引号/引号包裹的值：`- **上传**：密码 `upload_80894``
            make(
                "引号口令",
                r#"(?i)(?:用户名和密码|账号密码|登录密码|初始密码|密码|口令|password|passwd|pwd)\s*[`'"]([!-~]{4,64})[`'"]"#,
                1,
                None,
            ),
            // 表格行：`| 密码 | `xxx` |`（分隔符是竖线，不是冒号）
            make(
                "表格口令",
                r#"(?i)\|\s*(?:用户名|账号|密码|口令|凭据|密钥|令牌|token|key|secret|user|username|password|passwd)\s*\|\s*[`'"]?([!-~]{4,64})[`'"]?\s*\|"#,
                1,
                None,
            ),
            make("bearer_token", r"\b[Bb]earer\s+([A-Za-z0-9._~+/=-]{10,})", 1, None),
            make(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
                0,
                None,
            ),
            make("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b", 0, None),
            make("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b", 0, None),
            make("github_token", r"\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9]{30,}\b", 0, None),
            make("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b", 0, None),
            // 带凭据的连接串：组1=用户，组2=密码。
            make(
                "cred_url",
                r"\b(?:postgres(?:ql)?|mysql|redis|mongodb(?:\+srv)?|amqp|ftp)://([^\s/:@]+):([^\s/@]+)@",
                2,
                None,
            ),
        ]
    })
}

/// 扫描 vault，产出迁移计划（值只在内存）。
pub fn plan(root: &Path, excludes: &[String]) -> Result<Plan> {
    Ok(Plan {
        entries: scan(root, excludes)?
            .into_iter()
            .map(PlanEntry::from)
            .collect(),
    })
}

/// 执行迁移：写凭据库 + 把笔记里的明文替换成凭据引用。
pub fn apply(
    root: &Path,
    store: &dyn crate::ports::storage::TaskStore,
    now: i64,
    excludes: &[String],
) -> Result<Applied> {
    let mut entries = Vec::new();
    let mut credentials = 0usize;
    let mut files = Vec::new();
    // 按文件分组、组内从后往前替换：改写一处会移动后面的字节偏移，
    // 倒序改写才能保证后面的区间仍然有效。
    let mut by_file: std::collections::HashMap<String, Vec<PlannedSecret>> =
        std::collections::HashMap::new();
    for secret in scan(root, excludes)? {
        let revision = store
            .document(CREDENTIAL_KIND, &secret.id)?
            .map(|doc| doc.revision);
        let written = store.put_document(
            CREDENTIAL_KIND,
            &secret.id,
            revision,
            &serde_json::json!({
                "value": secret.value,
                "label": secret.label,
                "updated_at": now,
            }),
        )?;
        if !written {
            bail!("凭据写入冲突：{}", secret.id);
        }
        credentials += 1;
        files.push(secret.path.clone());
        entries.push(PlanEntry::from(secret.clone()));
        by_file.entry(secret.path.clone()).or_default().push(secret);
    }
    for (path, mut secrets) in by_file {
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.span.0));
        rewrite_file(root, &path, &secrets)?;
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
            id: secret.id,
            label: secret.label,
        }
    }
}

/// 扫描并保留明文（apply 用；plan 走同一函数）。
fn scan(root: &Path, excludes: &[String]) -> Result<Vec<PlannedSecret>> {
    let mut files = Vec::new();
    collect_text_files(root, &mut files)?;
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        // 排除名单：路径含任一子串即跳过（研究笔记里的泄露样本等）。
        if excludes.iter().any(|needle| rel.contains(needle.as_str())) {
            continue;
        }
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "凭据".to_owned());
        let mut seen_values: Vec<String> = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let line_start = text
                .split_inclusive('\n')
                .take(idx)
                .map(str::len)
                .sum::<usize>();
            for extract in extractors() {
                for caps in extract.regex.captures_iter(line) {
                    let Some(value_match) = caps.get(extract.value_group) else {
                        continue;
                    };
                    let mut value = value_match.as_str().to_owned();
                    // 值的字符类把引号/反引号也算进去了（ASCII 可见字符范围），
                    // 这里把尾部的引号剥掉，区间同步收窄。
                    let mut value_end = value_match.end();
                    while let Some(last) = value.chars().last() {
                        if matches!(last, '`' | '"' | '\'') {
                            value.pop();
                            value_end -= last.len_utf8();
                        } else {
                            break;
                        }
                    }
                    if value.is_empty() || is_placeholder(&value) {
                        continue;
                    }
                    // 同一段文本被多条规则命中时只取一条（按值去重）。
                    if seen_values.contains(&value) {
                        continue;
                    }
                    seen_values.push(value.clone());
                    let key = extract
                        .key_group
                        .and_then(|group| caps.get(group))
                        .map(|m| m.as_str().to_owned());
                    let value_span = (line_start + value_match.start(), line_start + value_end);
                    // 值常被反引号/引号包着（「密码是 `xxx`」）——连引号一起替换，
                    // 免得留下 `凭据「x」` 这种嵌套。
                    let (mut start, mut end) = value_span;
                    let before = text[..start].chars().next_back();
                    let after = text[end..].chars().next();
                    if let (Some(before), Some(after)) = (before, after)
                        && (before == '`' || before == '"' || before == '\'')
                        && before == after
                    {
                        start -= before.len_utf8();
                        end += after.len_utf8();
                    }
                    let span = (start, end);
                    // 上下文取前一行（表格里的「用户 root」就在上一行）。
                    let context = text
                        .lines()
                        .nth(idx.saturating_sub(1))
                        .unwrap_or("")
                        .to_owned();
                    let label = derive_label(line, &value, key.as_deref(), &context, &stem);
                    out.push(PlannedSecret {
                        path: rel.clone(),
                        line: idx + 1,
                        rule: extract.rule,
                        id: crate::domain::credential::new_id(),
                        label,
                        value,
                        span,
                        value_span,
                    });
                }
            }
        }
    }
    Ok(out)
}

/// 把一个文件里的明文按倒序替换成 `凭据「名字」`（一次读写）。
fn rewrite_file(root: &Path, rel: &str, secrets: &[PlannedSecret]) -> Result<()> {
    let path = root.join(rel);
    let mut text =
        std::fs::read_to_string(&path).with_context(|| format!("读 {}", path.display()))?;
    for secret in secrets {
        if secret.span.1 > text.len()
            || !text.is_char_boundary(secret.span.0)
            || secret.value_span.1 > text.len()
            || !text.is_char_boundary(secret.value_span.0)
        {
            continue; // 文件已被其他写者改动，跳过这一条。
        }
        // 二次确认区间确实是原值（防止错位改写别人的内容）。
        if text.get(secret.value_span.0..secret.value_span.1) != Some(secret.value.as_str()) {
            continue;
        }
        text.replace_range(
            secret.span.0..secret.span.1,
            &format!("凭据「{}」", secret.id),
        );
    }
    std::fs::write(&path, text).with_context(|| format!("写 {}", path.display()))?;
    Ok(())
}

/// 从上下文行里找主体（这条凭据属于谁）：
/// 表格行 `| 用户 | root |` → root；列表行 `- 用户名：`windy`` → windy。
fn subject_from_context(context: &str) -> Option<String> {
    for line in context.lines().rev().take(3) {
        // 表格：| 用户 | root |
        if line.contains('|') {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            let mut iter = cells.iter().filter(|cell| !cell.is_empty());
            if let (Some(label), Some(value)) = (iter.next(), iter.next())
                && is_subject_label(label)
            {
                let cleaned = value.trim_matches(['`', '"', '\'']).trim();
                if !cleaned.is_empty() && cleaned.chars().count() <= 24 {
                    return Some(cleaned.to_owned());
                }
            }
        }
        // 列表：- 用户名：`windy`
        if (line.contains("用户名") || line.contains("账号") || line.contains("用户"))
            && let Some((_, rest)) = line.split_once(['：', ':', '='])
        {
            let cleaned = rest.trim().trim_matches(['`', '"', '\'']).trim();
            if !cleaned.is_empty() && cleaned.chars().count() <= 24 {
                return Some(cleaned.to_owned());
            }
        }
    }
    None
}

/// 「用户/账号/用户名」这类主体标签。
fn is_subject_label(label: &str) -> bool {
    const SUBJECTS: &[&str] = &[
        "用户",
        "账号",
        "帐号",
        "用户名",
        "user",
        "username",
        "account",
    ];
    SUBJECTS.iter().any(|s| label.eq_ignore_ascii_case(s))
}

/// 是否是「密码/口令/token」这类无信息量的关键词。
fn is_generic_label(label: &str) -> bool {
    const GENERIC: &[&str] = &[
        "密码", "口令", "凭据", "token", "apikey", "api_key", "key", "secret", "password", "passwd",
    ];
    GENERIC
        .iter()
        .any(|generic| label.eq_ignore_ascii_case(generic))
}

/// 从上下文派生凭据名：行内 `**标签**` 优先，其次键名（mySQLPassword、
/// server_token），再次关键词前文；泛化名（密码/token）加文件主名前缀。
fn derive_label(line: &str, value: &str, key: Option<&str>, context: &str, stem: &str) -> String {
    let mut label = String::new();
    // 行内第一个粗体标签：「- **上传作业**：…」→「上传作业」。
    if let Some(start) = line.find("**")
        && let Some(end) = line[start + 2..].find("**")
    {
        label = line[start + 2..start + 2 + end].to_owned();
    }
    if label.is_empty()
        && let Some(key) = key
    {
        // 键名就是最准的名字：mySQLPassword、server_token、access_token。
        label = key.to_owned();
    }
    if label.is_empty() {
        // 表格行：`| 密码 | `xxx` |` → 取列名（密码）作标签。
        if let Some(pos) = line.find(value)
            && line.contains('|')
        {
            // 依次看值左侧的各个单元格，取第一个有内容的（跳过空单元格与引号）。
            let mut column = String::new();
            for cell in line[..pos].rsplit('|') {
                let cleaned = cell.trim().trim_matches(['`', '"', '\'']).trim();
                if !cleaned.is_empty() && cleaned.chars().count() <= 12 {
                    column = cleaned.to_owned();
                    break;
                }
            }
            // 列名太泛（密码）时用上下文主体区分：root-密码。
            if is_generic_label(&column) {
                label = match subject_from_context(context) {
                    Some(subject) => format!("{subject}-{column}"),
                    None => column,
                };
            } else {
                label = column;
            }
        }
    }
    if label.is_empty() {
        // 退而求其次：值之前的最后一个词组（「我的 ftp 密码是」→ ftp）。
        if let Some(pos) = line.find(value) {
            let cleaned = line[..pos]
                .trim_end_matches(|c: char| {
                    c.is_whitespace() || "：:（(【[]「」\"'`*—-,、是均为".contains(c)
                })
                .to_owned();
            let parts: Vec<&str> = cleaned
                .split(|c: char| c.is_whitespace() || "，,。；;、".contains(c))
                .filter(|part| !part.is_empty())
                .collect();
            let last = parts.last().copied().unwrap_or("");
            // 末段是「密码/口令」这类关键词时，名字取前一段（「ftp 密码」→ ftp）。
            if parts.len() >= 2 && is_generic_label(last) {
                let previous = parts[parts.len() - 2];
                if previous.chars().count() >= 2
                    && previous
                        .chars()
                        .any(|c| c.is_alphanumeric() || !c.is_ascii())
                {
                    label = previous.to_owned();
                } else {
                    label = last.to_owned();
                }
            } else {
                label = last.to_owned();
            }
        }
    }
    // 泛化名补文件主名前缀，避免「密码」这种毫无信息量的名字。
    const GENERIC: &[&str] = &[
        "密码", "口令", "token", "apikey", "api_key", "key", "secret", "password", "passwd", "凭据",
    ];
    if label.is_empty()
        || GENERIC
            .iter()
            .any(|generic| label.eq_ignore_ascii_case(generic))
    {
        // 泛用名（密码/token）优先用上下文主体区分，其次才退到文件主名。
        let prefix = subject_from_context(context).unwrap_or_else(|| stem.to_owned());
        label = format!("{prefix}-{label}");
    }
    let mut name: String = label
        .chars()
        .filter(|c| {
            !c.is_whitespace() && !matches!(c, '*' | '`' | '"' | '\'' | '|' | '>' | '<' | '=' | ':')
        })
        .take(24)
        .collect();
    if name.is_empty() {
        name = "凭据".to_owned();
    }
    name
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
    fn derives_label_from_bold_key_and_dedupes() {
        assert_eq!(
            derive_label(
                "- **上传作业**：`ftp://172.16.3.242`（用户名和密码都是 upload_80894）",
                "upload_80894",
                None,
                "",
                "note"
            ),
            "上传作业"
        );
        // 键值形态用键名。
        assert_eq!(
            derive_label(
                "mySQLPassword: hunter2secret",
                "hunter2secret",
                Some("mySQLPassword"),
                "",
                "README"
            ),
            "mySQLPassword"
        );
        // 泛化名补文件主名。
        assert_eq!(
            derive_label(
                "- 密码：`windy123`",
                "windy123",
                None,
                "- 用户名：`windy`",
                "AI渗透测试Agent使用指南"
            ),
            // 上下文主体优先：这条密码属于 windy。
            "windy-密码"
        );
        // 标签允许重复——寻址靠唯一标识，标签只是给人看。
        assert_eq!(
            derive_label("ftp 密码是 abc123", "abc123", None, "", "note"),
            "ftp"
        );
    }

    #[test]
    fn apply_rewrites_multiple_secrets_in_one_file() {
        use crate::adapters::database::sqlite::SqliteTaskStore;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "- **A**：密码是 aaa1111\n- **B**：密码是 bbb2222\n",
        )
        .unwrap();
        let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
        let applied = apply(&vault, &store, 0, &[]).unwrap();
        assert_eq!(applied.credentials, 2);
        let text = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert!(!text.contains("aaa1111"), "{text}");
        assert!(!text.contains("bbb2222"), "{text}");
        assert!(text.contains("凭据「cred_"), "{text}");
        assert_eq!(text.matches("凭据「cred_").count(), 2, "{text}");
    }

    #[test]
    fn quoted_value_replacement_eats_the_quotes() {
        use crate::adapters::database::sqlite::SqliteTaskStore;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "- **上传作业**：（用户名和密码都是 `upload_80894`），找到文件夹\n",
        )
        .unwrap();
        let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
        let applied = apply(&vault, &store, 0, &[]).unwrap();
        assert_eq!(applied.credentials, 1);
        let text = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert!(!text.contains("upload_80894"), "{text}");
        // 引号一起吃掉，不留 `凭据「x」` 的嵌套。
        assert!(text.contains("凭据「cred_"), "{text}");
        assert!(!text.contains("`凭据"), "{text}");
    }

    #[test]
    fn table_row_secret_is_found_and_named_after_column() {
        use crate::adapters::database::sqlite::SqliteTaskStore;
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "| 地址 | `ssh -p 34609 root@t1.sjcmc.cn` |\n| 密码 | `LZWQ2XFUiYBP` |\n",
        )
        .unwrap();
        let plan = plan(&vault, &[]).unwrap();
        assert_eq!(plan.entries.len(), 1, "{plan:?}");
        assert_eq!(plan.entries[0].rule, "表格口令");
        assert_eq!(plan.entries[0].label, "a-密码");
        let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
        let applied = apply(&vault, &store, 0, &[]).unwrap();
        assert_eq!(applied.credentials, 1);
        let text = std::fs::read_to_string(vault.join("a.md")).unwrap();
        assert!(!text.contains("LZWQ2XFUiYBP"), "{text}");
        assert!(text.contains("凭据「cred_"), "{text}");
    }

    #[test]
    fn table_secret_uses_subject_from_previous_row() {
        // 运维笔记：| 用户 | root | 下面跟 | 密码 | `xxx` | → root-密码
        let label = derive_label(
            "| 密码 | `LZWQ2XFUiYBP` |",
            "LZWQ2XFUiYBP",
            None,
            "| 用户 | root |",
            "服务器部署信息",
        );
        assert_eq!(label, "root-密码");
        // 列表行自带具体词：`- Redis 密码：`xxx`` → Redis
        let redis = derive_label(
            "- Redis 密码：`redis_xZR4FC`",
            "redis_xZR4FC",
            None,
            "",
            "服务器部署信息",
        );
        assert_eq!(redis, "Redis");
    }

    #[test]
    fn placeholders_are_not_migrated() {
        assert!(is_placeholder("********"));
        assert!(is_placeholder("your_password_here"));
        assert!(is_placeholder("TODO"));
        assert!(!is_placeholder("upload_80894"));
        assert!(!is_placeholder("windy123"));
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
        let plan = plan(&vault, &[]).unwrap();
        assert_eq!(plan.entries.len(), 1, "{plan:?}");
        assert_eq!(plan.entries[0].label, "上传作业");
        assert!(plan.entries[0].id.starts_with("cred_"), "{plan:?}");
    }

    #[test]
    fn excludes_skip_matching_paths() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(vault.join("leak.md"), "server_token=abcdef1234567890\n").unwrap();
        assert_eq!(plan(&vault, &[]).unwrap().entries.len(), 1);
        assert!(
            plan(&vault, &["leak.md".to_owned()])
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[test]
    fn questions_and_lookalikes_are_not_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        std::fs::create_dir_all(&vault).unwrap();
        std::fs::write(
            vault.join("a.md"),
            "我的密码是多少来着\n密码 6 位\n密码学原理\n密码 bucket 机制\nmySQLPassword: \"********\"\n",
        )
        .unwrap();
        assert!(plan(&vault, &[]).unwrap().entries.is_empty());
    }
}
