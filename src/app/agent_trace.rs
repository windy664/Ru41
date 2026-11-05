//! Agent 运行轨迹汇总：读底座事件日志，说清「这轮用了什么工具、读了哪些技能包、
//! 改了哪些文件」。
//!
//! 数据源是 run_dir/events.jsonl —— codex 适配器一直在写（见
//! adapters/agent/codex/exec.rs），pi 适配器从 2026-11-05 起也写（见
//! adapters/agent/pi/rpc.rs）。两种底座的事件结构不同，这里都认：
//!
//! - codex：`item.started` / `item.completed`，`item.type` 取
//!   `command_execution`（带 command）、`agent_message`（带 text）、
//!   `file_change`（带改动文件）。
//! - pi：`tool_execution_start` 一类事件，工具名在 `tool` / `name` / `tool_name`
//!   字段上，参数在 `arguments` / `args` 上。
//!
//! 用途：回答"它为什么没找到答案"——最典型的就是模型去下载并渲染大 PDF。
//! 失败通知里带上这段汇总，排障就不用再去翻日志。

use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// 一次运行的轨迹汇总。
#[derive(Debug, Default, Clone, Serialize)]
pub struct TraceSummary {
    /// 用到的工具（底座工具名，或命令的第一个词），按出现次数降序。
    pub tools: Vec<(String, usize)>,
    /// 读过的技能包（从命令里出现 `SKILL.md` / `skills/<名>` 推断）。
    pub skills: Vec<String>,
    /// 改动的文件（相对路径）。
    pub changed_files: Vec<String>,
    /// agent 说了几句话。
    pub messages: usize,
    /// 完整命令（去重后截断，最多 10 条），便于看它到底搜了什么。
    pub commands: Vec<String>,
}

/// 汇总一个 run 目录下的底座事件文件。
pub fn summarize_run(run_dir: &Path) -> Result<TraceSummary> {
    summarize_events(&run_dir.join("events.jsonl"))
}

/// 汇总底座事件文件（两种底座结构都认）。
pub fn summarize_events(path: &Path) -> Result<TraceSummary> {
    let mut tools: BTreeMap<String, usize> = BTreeMap::new();
    let mut skills: BTreeMap<String, ()> = BTreeMap::new();
    let mut changed: BTreeMap<String, ()> = BTreeMap::new();
    let mut commands: Vec<String> = Vec::new();
    let mut messages = 0usize;

    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(TraceSummary::default());
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // codex 形态：item.completed（item.started 与它成对重复，同命令会数两次）
        if event.get("type").and_then(|v| v.as_str()) == Some("item.completed")
            && let Some(item) = event.get("item")
        {
            match item.get("type").and_then(|t| t.as_str()) {
                Some("command_execution") => {
                    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
                        record_command(cmd, &mut tools, &mut skills, &mut commands);
                    }
                }
                Some("agent_message") => messages += 1,
                Some("file_change") => {
                    for key in ["path", "file", "filename", "paths"] {
                        match item.get(key) {
                            Some(serde_json::Value::String(path)) => {
                                changed.insert(path.clone(), ());
                            }
                            Some(serde_json::Value::Array(paths)) => {
                                for path in paths.iter().filter_map(|p| p.as_str()) {
                                    changed.insert(path.to_string(), ());
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        // pi 形态：顶层事件（tool_execution_start 带 toolCallId/toolName/args）
        match event.get("type").and_then(|t| t.as_str()) {
            Some("tool_execution_start") | Some("tool_call") | Some("tool_use") => {
                let name = event
                    .get("toolName")
                    .or_else(|| event.get("tool"))
                    .or_else(|| event.get("tool_name"))
                    .or_else(|| event.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool")
                    .to_string();
                *tools.entry(name).or_default() += 1;
                let args = event
                    .get("args")
                    .or_else(|| event.get("arguments"))
                    .map(|value| match value {
                        serde_json::Value::String(text) => text.clone(),
                        serde_json::Value::Object(_) => value.to_string(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                if !args.is_empty() {
                    // 字符串参数可能是整条命令 → 计入工具与命令；
                    // 对象参数只用来认技能包（否则会造出假工具名）。
                    if event
                        .get("args")
                        .or_else(|| event.get("arguments"))
                        .map(|v| v.is_string())
                        .unwrap_or(false)
                    {
                        record_command(&args, &mut tools, &mut skills, &mut commands);
                    } else {
                        scan_skills(&args, &mut skills);
                    }
                }
            }
            Some("message_end") => {
                let text = event
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .map(|content| match content {
                        serde_json::Value::String(text) => text.clone(),
                        serde_json::Value::Array(blocks) => blocks
                            .iter()
                            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                if !text.trim().is_empty() {
                    messages += 1;
                }
            }
            Some("assistant_message") | Some("message") => messages += 1,
            _ => {}
        }
    }

    let mut tools: Vec<(String, usize)> = tools.into_iter().collect();
    tools.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(TraceSummary {
        tools,
        skills: skills.into_keys().collect(),
        changed_files: changed.into_keys().collect(),
        messages,
        commands: commands.into_iter().take(10).collect(),
    })
}

/// 只从文本里认技能包（不计入工具与命令）——pi 的工具参数是 JSON 对象，
/// 整段当命令统计会造出一个假工具名。
fn scan_skills(text: &str, skills: &mut BTreeMap<String, ()>) {
    for part in text.split_whitespace() {
        let cleaned = part.trim_matches(['"', '\'', ',', ';', ':', '{', '}']);
        if let Some(rest) = cleaned.strip_suffix("SKILL.md") {
            let name = rest
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(rest);
            if !name.is_empty() {
                skills.insert(name.to_string(), ());
            }
        }
        if let Some(idx) = cleaned.find("skills/") {
            let rest = &cleaned[idx + "skills/".len()..];
            let name = rest.split('/').next().unwrap_or("").trim_matches('"');
            if !name.is_empty() && !name.contains('.') {
                skills.insert(name.to_string(), ());
            }
        }
    }
}

/// 记一条命令：工具名按命令首词计，技能包从路径里认。
fn record_command(
    command: &str,
    tools: &mut BTreeMap<String, usize>,
    skills: &mut BTreeMap<String, ()>,
    commands: &mut Vec<String>,
) {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return;
    }
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let head = parts.first().copied().unwrap_or(trimmed);
    // bash -lc "..." 这类包装，真实命令在引号里。注意开关可能是 -c / -lc / -lic，
    // 不能按 "-c" 子串切（"-lc" 里并没有 "-c"）。
    let head = if matches!(head, "bash" | "sh" | "zsh") {
        parts
            .iter()
            .position(|token| token.starts_with('-') && token.contains('c'))
            .and_then(|idx| parts.get(idx + 1))
            .map(|token| token.trim_matches(['"', '\'', '\'', '`']))
            .filter(|token| !token.is_empty())
            .unwrap_or(head)
    } else {
        head
    };
    *tools.entry(head.to_string()).or_default() += 1;
    for part in trimmed.split_whitespace() {
        let cleaned = part.trim_matches(['"', '\'', ',', ';']);
        if let Some(rest) = cleaned.strip_suffix("SKILL.md") {
            let name = rest
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(rest);
            if !name.is_empty() {
                skills.insert(name.to_string(), ());
            }
        }
        // skills/<名>/ 形式
        if let Some(idx) = cleaned.find("skills/") {
            let rest = &cleaned[idx + "skills/".len()..];
            let name = rest.split('/').next().unwrap_or("").trim_matches('"');
            if !name.is_empty() && !name.contains('.') {
                skills.insert(name.to_string(), ());
            }
        }
    }
    let short: String = trimmed.chars().take(160).collect();
    if !commands.iter().any(|c| c == &short) {
        commands.push(short);
    }
}

/// 汇总渲染成一行行文本（给 QQ 用）。
pub fn render(summary: &TraceSummary) -> Vec<String> {
    let mut lines = Vec::new();
    if !summary.tools.is_empty() {
        let tools: Vec<String> = summary
            .tools
            .iter()
            .map(|(name, count)| {
                if *count > 1 {
                    format!("{name}×{count}")
                } else {
                    name.clone()
                }
            })
            .collect();
        lines.push(format!("工具：{}", tools.join("、")));
    }
    if !summary.skills.is_empty() {
        lines.push(format!("技能：{}", summary.skills.join("、")));
    }
    if !summary.changed_files.is_empty() {
        let files: Vec<String> = summary
            .changed_files
            .iter()
            .take(5)
            .map(|f| f.rsplit('/').next().unwrap_or(f).to_string())
            .collect();
        lines.push(format!("改动文件：{}", files.join("、")));
    }
    if !summary.commands.is_empty() {
        lines.push(format!("执行过的命令：{}", summary.commands.join(" / ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_codex_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"thread.started","thread_id":"t1"}"#,
                "\n",
                r#"{"type":"item.completed","item":{"id":"i0","type":"agent_message","text":"好的"}}"#,
                "\n",
                r#"{"type":"item.started","item":{"id":"i1","type":"command_execution","command":"bash -lc \"rg -n '单项奖励金' /vault\""}}"#,
                "\n",
                r#"{"type":"item.completed","item":{"id":"i1","type":"command_execution","command":"bash -lc \"rg -n '单项奖励金' /vault\""}}"#,
                "\n",
                r#"{"type":"item.started","item":{"id":"i2","type":"command_execution","command":"rg -n 励志金 /vault"}}"#,
                "\n",
                r#"{"type":"item.completed","item":{"id":"i2","type":"command_execution","command":"rg -n 励志金 /vault"}}"#,
                "\n",
                r#"{"type":"item.completed","item":{"id":"i3","type":"file_change","path":"/vault/a.md"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let summary = summarize_events(&path).unwrap();
        assert_eq!(summary.messages, 1);
        assert!(summary.tools.iter().any(|(n, c)| n == "rg" && *c == 2));
        assert_eq!(summary.changed_files, vec!["/vault/a.md".to_string()]);
        assert!(!summary.commands.is_empty());
    }

    #[test]
    fn recognizes_skill_packages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"item.completed","item":{"type":"command_execution","command":"cat /usr/share/ru41/plugins/obsidian/skills/vault-search/SKILL.md"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let summary = summarize_events(&path).unwrap();
        assert!(
            summary.skills.contains(&"vault-search".to_string()),
            "{:?}",
            summary.skills
        );
    }

    #[test]
    fn summarizes_pi_tool_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"read","args":{"path":"/vault/notes.md"}}"#,
                "\n",
                r#"{"type":"tool_execution_start","toolCallId":"c2","toolName":"read","args":{"path":"/vault/other.md"}}"#,
                "\n",
            ),
        )
        .unwrap();
        let summary = summarize_events(&path).unwrap();
        assert_eq!(summary.tools, vec![("read".to_string(), 2)]);
    }

    #[test]
    fn missing_file_is_empty_summary() {
        let summary = summarize_events(Path::new("/nonexistent/events.jsonl")).unwrap();
        assert!(summary.tools.is_empty());
    }
}
