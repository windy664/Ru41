//! `ru41 tingwu` 的子命令。
//!
//! - `transcribe <文件>`：上传音视频 → 转写 → 落盘逐字稿（**主入口**）
//! - `list [条数]`：看账号里的历史转写任务
//! - `result <任务ID>`：取某个已有任务的逐字稿（不重新转写）
//!
//! ⚠️ **不能叫 `run`/`poll`**：宿主把 `ru41 <模块> poll|run` 两个词**留给 tracker**
//! （`poll` 跑一轮，`run` 起常驻循环，见 src/main.rs 的 Command::Tracker 分发），
//! 插件自己的钩子只拿得到其余子命令。
//!
//! 输出落在 `output_dir`（默认 state.db 同级的 `tingwu/`，容器内即 `/data/tingwu`）：
//! `<任务ID>.md` 逐字稿（说话人 + 时间戳）、`<任务ID>.json` 原始逐字稿，
//! 外加 `--format text|srt` 时的 `.txt` / `.srt`。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};

use crate::client::{TingwuClient, Transcript, render_markdown, render_srt, render_text};
use crate::config::TingwuSection;

pub const MODULE_NAME: &str = "tingwu";

/// 输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Markdown,
    Text,
    Srt,
}

impl Format {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "markdown" | "md" => Some(Self::Markdown),
            "text" | "txt" | "plain" => Some(Self::Text),
            "srt" => Some(Self::Srt),
            _ => None,
        }
    }
}

/// 注册进宿主的 CLI 入口（签名由 `ModuleDef.cli` 定）。
pub fn tingwu_cli(
    config: &ru41::bootstrap::config::Config,
    store: Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>>
{
    let section = config.sources.get_or_default::<TingwuSection>(MODULE_NAME);
    let output_dir = output_dir(config, &section);
    let args: Vec<String> = args.to_vec();
    Box::pin(ru41::plugins::bridge(async move {
        let value = dispatch(&section, &output_dir, store.as_ref(), &args).await?;
        Ok(Some(value))
    }))
}

/// 逐字稿落盘目录：配置优先，否则 state.db 同级（容器内 `/data`）。
fn output_dir(config: &ru41::bootstrap::config::Config, section: &TingwuSection) -> PathBuf {
    section.output_dir.clone().unwrap_or_else(|| {
        config
            .store
            .path
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("/data"))
            .join("tingwu")
    })
}

/// 从凭据托管库取听悟 cookie（真值只在这里出现，不进模型上下文）。
fn cookie_from_store(store: &dyn ru41::ports::storage::TaskStore, label: &str) -> Result<String> {
    let doc = ru41::domain::credential::resolve(store, label)?.ok_or_else(|| {
        anyhow::anyhow!(
            "凭据托管库里没有名为「{label}」的凭据（听悟 cookie）。\
             先存一条：ru41 secret set {label} '<整串 cookie>'"
        )
    })?;
    let value = doc.value["value"].as_str().unwrap_or("").trim().to_owned();
    if value.is_empty() {
        bail!("凭据「{label}」是空的");
    }
    Ok(value)
}

async fn dispatch(
    section: &TingwuSection,
    output_dir: &Path,
    store: &dyn ru41::ports::storage::TaskStore,
    args: &[String],
) -> Result<serde_json::Value> {
    let command = args
        .first()
        .map(|raw| raw.trim().to_lowercase())
        .unwrap_or_else(|| "help".to_owned());
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();

    match command.as_str() {
        "transcribe" | "转写" | "转录" => {
            let (path, format) = parse_transcribe_args(&rest)?;
            let client = build_client(section, store)?;
            let transcript = client
                .transcribe(&path)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            write_outputs(output_dir, &transcript, format).await
        }
        "result" | "结果" | "稿子" => {
            let (trans_id, format) = parse_result_args(&rest)?;
            let client = build_client(section, store)?;
            let title = trans_id.to_owned();
            let transcript = client
                .result(&trans_id, &title)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            write_outputs(output_dir, &transcript, format).await
        }
        "list" | "列表" | "历史" => {
            let limit = rest
                .first()
                .and_then(|raw| raw.trim().parse::<u64>().ok())
                .unwrap_or(10)
                .clamp(1, 100);
            let client = build_client(section, store)?;
            let tasks = client
                .list(limit)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let rows: Vec<serde_json::Value> = tasks
                .iter()
                .map(|task| {
                    serde_json::json!({
                        "trans_id": task.trans_id,
                        "task_id": task.task_id,
                        "show_name": task.show_name,
                        "status": task.status,
                        "duration_secs": task.duration_secs,
                        "word_count": task.word_count,
                        "show_time": crate::client::format_ts(task.show_time_ms),
                    })
                })
                .collect();
            Ok(serde_json::json!({ "ok": true, "count": rows.len(), "tasks": rows }))
        }
        "help" | "--help" | "-h" => Ok(serde_json::json!({
            "ok": true,
            "usage": [
                "ru41 tingwu transcribe <音视频文件> [--format markdown|text|srt]",
                "ru41 tingwu result <任务ID> [--format markdown|text|srt]",
                "ru41 tingwu list [条数]",
            ],
        })),
        other => Ok(serde_json::json!({
            "ok": false,
            "error": format!("未知子命令 {other}（可用：transcribe / result / list）")
        })),
    }
}

/// 构造客户端（取 cookie + 组装参数）。
fn build_client(
    section: &TingwuSection,
    store: &dyn ru41::ports::storage::TaskStore,
) -> Result<TingwuClient> {
    let cookie = cookie_from_store(store, section.credential())?;
    TingwuClient::new(&cookie, section)
}

/// 解析 `transcribe` 的参数：文件路径 + 可选 `--format`。
fn parse_transcribe_args(args: &[String]) -> Result<(PathBuf, Format)> {
    let mut path: Option<PathBuf> = None;
    let mut format = Format::Markdown;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].trim();
        if let Some(value) = arg.strip_prefix("--format=") {
            format = Format::parse(value).ok_or_else(|| {
                anyhow::anyhow!("未知格式 {value}（可用：markdown / text / srt）")
            })?;
        } else if arg == "--format" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("--format 后面要给值"))?;
            format = Format::parse(value).ok_or_else(|| {
                anyhow::anyhow!("未知格式 {value}（可用：markdown / text / srt）")
            })?;
            index += 1;
        } else if arg.starts_with('-') {
            bail!("未知参数 {arg}");
        } else if path.is_none() {
            path = Some(PathBuf::from(arg));
        } else {
            bail!("只能给一个文件（多余的是 {arg}）");
        }
        index += 1;
    }
    let path = path.ok_or_else(|| anyhow::anyhow!("用法：ru41 tingwu transcribe <音视频文件>"))?;
    Ok((path, format))
}

/// 解析 `result` 的参数：任务 ID + 可选 `--format`。
fn parse_result_args(args: &[String]) -> Result<(String, Format)> {
    let mut trans_id: Option<String> = None;
    let mut format = Format::Markdown;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].trim();
        if let Some(value) = arg.strip_prefix("--format=") {
            format = Format::parse(value).ok_or_else(|| {
                anyhow::anyhow!("未知格式 {value}（可用：markdown / text / srt）")
            })?;
        } else if arg == "--format" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("--format 后面要给值"))?;
            format = Format::parse(value).ok_or_else(|| {
                anyhow::anyhow!("未知格式 {value}（可用：markdown / text / srt）")
            })?;
            index += 1;
        } else if arg.starts_with('-') {
            bail!("未知参数 {arg}");
        } else if trans_id.is_none() {
            trans_id = Some(arg.to_owned());
        } else {
            bail!("只能给一个任务 ID（多余的是 {arg}）");
        }
        index += 1;
    }
    let trans_id = trans_id.ok_or_else(|| anyhow::anyhow!("用法：ru41 tingwu result <任务ID>"))?;
    Ok((trans_id, format))
}

/// 落盘：`.md` + `.json` 总是写；`--format text|srt` 时额外写对应文件。
async fn write_outputs(
    output_dir: &Path,
    transcript: &Transcript,
    format: Format,
) -> Result<serde_json::Value> {
    tokio::fs::create_dir_all(output_dir)
        .await
        .with_context(|| format!("建目录 {} 失败", output_dir.display()))?;

    let stem = format!("{}-{}", transcript.trans_id, sanitize(&transcript.title));
    let markdown_path = output_dir.join(format!("{stem}.md"));
    let json_path = output_dir.join(format!("{stem}.json"));
    tokio::fs::write(&markdown_path, render_markdown(transcript))
        .await
        .with_context(|| format!("写 {} 失败", markdown_path.display()))?;
    tokio::fs::write(&json_path, serde_json::to_string_pretty(&transcript.raw)?)
        .await
        .with_context(|| format!("写 {} 失败", json_path.display()))?;

    let mut files = vec![
        markdown_path.display().to_string(),
        json_path.display().to_string(),
    ];
    match format {
        Format::Markdown => {}
        Format::Text => {
            let path = output_dir.join(format!("{stem}.txt"));
            tokio::fs::write(&path, render_text(transcript))
                .await
                .with_context(|| format!("写 {} 失败", path.display()))?;
            files.push(path.display().to_string());
        }
        Format::Srt => {
            let path = output_dir.join(format!("{stem}.srt"));
            tokio::fs::write(&path, render_srt(transcript))
                .await
                .with_context(|| format!("写 {} 失败", path.display()))?;
            files.push(path.display().to_string());
        }
    }

    let preview: String = transcript.full_text().chars().take(200).collect();
    Ok(serde_json::json!({
        "ok": true,
        "trans_id": transcript.trans_id,
        "title": transcript.title,
        "duration_secs": transcript.duration_secs,
        "word_count": transcript.word_count,
        "segments": transcript.segments.len(),
        "speakers": transcript.speaker_count(),
        "files": files,
        "markdown_path": markdown_path.display().to_string(),
        "preview": preview,
    }))
}

/// 文件名里能留的字符（别的换成 `_`）。
fn sanitize(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "audio".to_owned()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_transcribe_arguments() {
        let args = vec![
            "/data/a.mp3".to_owned(),
            "--format".to_owned(),
            "srt".to_owned(),
        ];
        let (path, format) = parse_transcribe_args(&args).expect("应解析成功");
        assert_eq!(path, PathBuf::from("/data/a.mp3"));
        assert_eq!(format, Format::Srt);
    }

    #[test]
    fn parses_transcribe_arguments_with_equals() {
        let args = vec!["/data/a.mp3".to_owned(), "--format=text".to_owned()];
        let (_, format) = parse_transcribe_args(&args).expect("应解析成功");
        assert_eq!(format, Format::Text);
    }

    #[test]
    fn missing_path_is_an_error() {
        let args = vec!["--format".to_owned(), "srt".to_owned()];
        assert!(parse_transcribe_args(&args).is_err());
    }

    #[test]
    fn unknown_format_is_an_error() {
        let args = vec![
            "/data/a.mp3".to_owned(),
            "--format".to_owned(),
            "pdf".to_owned(),
        ];
        assert!(parse_transcribe_args(&args).is_err());
    }

    #[test]
    fn sanitizes_file_names() {
        // title 传进来的是**不带扩展名**的文件干（transcribe 走 rsplit_once('.')），
        // 这里只负责把不能进文件名的字符换掉。
        assert_eq!(sanitize("我的 录音"), "我的_录音");
        assert_eq!(sanitize("speech"), "speech");
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize("///"), "audio");
    }
}
