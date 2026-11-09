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
    for secret in scan(root, excludes)? {
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
fn scan(root: &Path, excludes: &[String]) -> Result<Vec<PlannedSecret>> {
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
                    let value = value_match.as_str().to_owned();
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
                    let span = (
                        line_start + value_match.start(),
                        line_start + value_match.end(),
                    );
                    let name = derive_name(line, &value, key.as_deref(), &stem, &used_names);
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

/// 占位符/掩码值不迁移：`********`、`xxxxxx`、`your_password`、`TODO` 之类。
fn is_placeholder(value: &str) -> bool {
    const WORDS: &[&str] = &[
        "your_password",
        "yourpassword",
        "changeme",
        "todo",
        "example",
        "placeholder",
        "xxxx",
        "***",
    ];
    let lower = value.to_lowercase();
    if WORDS.iter().any(|word| lower.contains(word)) {
        return true;
    }
    // 同一字符连续出现 3 次以上（********、-------）。
    let chars: Vec<char> = value.chars().collect();
    chars.len() >= 3
        && chars
            .windows(3)
            .any(|window| window.iter().all(|c| *c == window[0]))
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
fn derive_name(line: &str, value: &str, key: Option<&str>, stem: &str, used: &[String]) -> String {
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
        label = format!("{stem}-{label}");
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
    fn derives_label_from_bold_key_and_dedupes() {
        assert_eq!(
            derive_name(
                "- **上传作业**：`ftp://172.16.3.242`（用户名和密码都是 upload_80894）",
                "upload_80894",
                None,
                "note",
                &[]
            ),
            "上传作业"
        );
        // 键值形态用键名。
        assert_eq!(
            derive_name(
                "mySQLPassword: hunter2secret",
                "hunter2secret",
                Some("mySQLPassword"),
                "README",
                &[]
            ),
            "mySQLPassword"
        );
        // 泛化名补文件主名。
        assert_eq!(
            derive_name(
                "- 密码：`windy123`",
                "windy123",
                None,
                "AI渗透测试Agent使用指南",
                &[]
            ),
            "AI渗透测试Agent使用指南-密码"
        );
        // 同名第二个加序号。
        let used = vec!["ftp".to_owned()];
        assert_eq!(
            derive_name("ftp 密码是 abc123", "abc123", None, "note", &used),
            "ftp-2"
        );
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
        assert_eq!(plan.entries[0].name, "上传作业");
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
