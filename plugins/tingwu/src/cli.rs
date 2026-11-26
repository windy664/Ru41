//! `ru41 tingwu` 的子命令。
//!
//! - `transcribe <文件>`：上传音视频 → 转写 → 落盘逐字稿（**主入口**）
//! - `list [条数]`：看账号里的历史转写任务
//! - `result <任务ID>`：取某个已有任务的逐字稿（不重新转写）
//! - `quota`：看账号还剩多少转写时长 / 存储
//! - `clean`：清云端旧转写 + 本地旧稿（**默认只列不删**，要 `--yes` 才真删）
//!
//! ⚠️ **不能叫 `run`/`poll`**：宿主把 `ru41 <模块> poll|run` 两个词**留给 tracker**
//! （`poll` 跑一轮，`run` 起常驻循环，见 src/main.rs 的 Command::Tracker 分发），
//! 插件自己的钩子只拿得到其余子命令。
//!
//! 输出落在 `output_dir`（默认 state.db 同级的 `tingwu/`，容器内即 `/data/tingwu`）：
//! `<任务ID>.md` 逐字稿（说话人 + 时间戳）、`<任务ID>.json` 原始逐字稿，
//! 外加 `--format text|srt` 时的 `.txt` / `.srt`。
//!
//! **兜底链**：听悟失败（配额用完 / cookie 过期 / 网络抽风）→ 自动回落本地 ASR
//! （`plugins/lecture` 那套 whisper.cpp / paraformer），返回里会标 `engine=local-fallback`。
//! 本地稿**没有说话人分离**，只有字。

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
pub fn cookie_from_store(
    store: &dyn ru41::ports::storage::TaskStore,
    label: &str,
) -> Result<String> {
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
            let (path, format, text_out) = parse_transcribe_args(&rest)?;
            // --text-only：只出纯文本（给别的插件/脚本调），不落盘逐字稿。
            // --text-out <路径>：纯文本写到该文件（**子进程调用走这条**：
            //   宿主 CLI 会把插件返回值打成 JSON，stdout 混不进纯文本）。
            // --no-quota-check：跳过额度预检（默认要查）。
            // --no-fallback：听悟失败**不**在插件内回落本地（由调用方决定兜底）。
            let text_only = rest.iter().any(|arg| arg == "--text-only");
            let quota_check = !rest.iter().any(|arg| arg == "--no-quota-check");
            let fallback = section.fallback_local()
                && !rest.iter().any(|arg| arg == "--no-fallback");
            let client = build_client(section, store)?;

            // ── 额度预检（2026-11-26）：不够就别上传 ──────────────────────
            // 听悟按**音频时长**计费。额度不够时上传 70MB + 云端排队跑完还是失败，
            // 白跑一趟还占存储。先查额度，不够直接走本地 ASR。
            if quota_check {
                let duration = TingwuClient::media_duration_secs(&path);
                match client.quota_sufficient(duration.unwrap_or(0.0)).await {
                    Ok(Some(reason)) => {
                        if !fallback {
                            return Err(anyhow::anyhow!("{reason}"));
                        }
                        eprintln!("[tingwu] {reason}，改用本地 ASR");
                        let transcript = client.transcribe_local(&path).await.map_err(|local| {
                            anyhow::anyhow!("{reason}；本地兜底也失败：{local}")
                        })?;
                        return finish(
                            text_only,
                            text_out.as_deref(),
                            output_dir,
                            &transcript,
                            format,
                            Some(reason),
                            Some("local-fallback"),
                        )
                        .await;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        // 查额度本身失败（cookie 过期/网络问题）→ 让后面正常走，
                        // 转写失败会自然回落本地。
                        eprintln!("[tingwu] 查额度失败，继续尝试转写：{error:#}");
                    }
                }
            }

            match client.transcribe(&path).await {
                Ok(transcript) => {
                    finish(
                        text_only,
                        text_out.as_deref(),
                        output_dir,
                        &transcript,
                        format,
                        None,
                        None,
                    )
                    .await
                }
                Err(error) => {
                    // 听悟这条路挂了（配额用完 / cookie 过期 / 网络抽风）→ 回落本地 ASR。
                    // 本地没有说话人分离，但总比拿不到字强 —— 所以返回里标清楚是谁出的稿。
                    if !fallback {
                        return Err(anyhow::anyhow!("{error}"));
                    }
                    let reason = format!("{error}");
                    let transcript = client.transcribe_local(&path).await.map_err(|local| {
                        anyhow::anyhow!(
                            "听悟失败（{reason}），本地兜底也失败：{local}\n\
                                 两条路都断了 —— 听悟多半是配额用完或 cookie 过期，\
                                 先跑 `ru41 tingwu quota` 看看剩余时长"
                        )
                    })?;
                    finish(
                        text_only,
                        text_out.as_deref(),
                        output_dir,
                        &transcript,
                        format,
                        Some(reason),
                        Some("local-fallback"),
                    )
                    .await
                }
            }
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
                        "show_time": crate::client::format_time(task.show_time_ms),
                    })
                })
                .collect();
            Ok(serde_json::json!({ "ok": true, "count": rows.len(), "tasks": rows }))
        }
        "quota" | "配额" | "额度" => {
            let client = build_client(section, store)?;
            let quota = client
                .quota()
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(serde_json::json!({
                "ok": true,
                "user_name": quota.user_name,
                "remaining_hours": round2(quota.remaining_secs as f64 / 3600.0),
                "remaining_secs": quota.remaining_secs,
                "total_hours": round2(quota.total_secs as f64 / 3600.0),
                "used_hours": round2(quota.used_secs as f64 / 3600.0),
                "storage_remaining_gib": round2(
                    (quota.storage_total - quota.storage_used).max(0) as f64 / 1073741824.0
                ),
                "storage_total_gib": round2(quota.storage_total as f64 / 1073741824.0),
            }))
        }
        "clean" | "清理" => {
            let options = parse_clean_args(&rest)?;
            let client = build_client(section, store)?;
            clean(&client, output_dir, &options).await
        }
        "help" | "--help" | "-h" => Ok(serde_json::json!({
            "ok": true,
            "usage": [
                "ru41 tingwu transcribe <音视频文件> [--format markdown|text|srt]",
                "    [--text-out <路径>] [--no-fallback] [--no-quota-check]",
                "    # 默认先查额度：剩余时长不够就直接走本地 ASR，不白上传",
                "    # --text-out 把纯文本写到文件（给别的插件调用；宿主 CLI 会把返回值打成 JSON，stdout 混不进纯文本）",
                "ru41 tingwu result <任务ID> [--format markdown|text|srt]",
                "ru41 tingwu list [条数]",
                "ru41 tingwu quota            # 剩余转写时长 / 存储",
                "ru41 tingwu clean [--cloud] [--older-than-days N] [--keep N] [--permanent] [--yes]",
            ],
        })),
        other => Ok(serde_json::json!({
            "ok": false,
            "error": format!("未知子命令 {other}（可用：transcribe / result / list / quota / clean）")
        })),
    }
}

/// 转写收尾：要么落盘并回 JSON，要么（`--text-only`）只把纯文本打到 stdout。
///
/// `--text-only` 是给**别的插件**调用的出口：lecture 想用听悟转写时，
/// 跑 `ru41 tingwu transcribe <文件> --text-only`，从 stdout 拿纯文本 ——
/// 走进程边界，插件之间不需要共享任何内存状态（各自静态链接的 cdylib
/// 连全局变量都不共享）。
async fn finish(
    text_only: bool,
    text_out: Option<&Path>,
    output_dir: &Path,
    transcript: &Transcript,
    format: Format,
    fallback_reason: Option<String>,
    engine: Option<&str>,
) -> Result<serde_json::Value> {
    if text_only {
        let text = transcript.full_text();
        if let Some(path) = text_out {
            // 子进程调用走文件：宿主 CLI 会把返回值打成 JSON，stdout 混不进纯文本。
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.ok();
            }
            tokio::fs::write(path, &text)
                .await
                .with_context(|| format!("写文本文件 {} 失败", path.display()))?;
            return Ok(serde_json::json!({
                "ok": true,
                "text_only": true,
                "text_out": path.display().to_string(),
                "chars": text.chars().count(),
                "engine": engine.unwrap_or("tingwu"),
                "fallback_reason": fallback_reason,
            }));
        }
        println!("{text}");
        return Ok(serde_json::json!({
            "ok": true,
            "text_only": true,
            "chars": text.chars().count(),
            "engine": engine.unwrap_or("tingwu"),
            "fallback_reason": fallback_reason,
        }));
    }
    let mut value = write_outputs(output_dir, transcript, format).await?;
    if let Some(obj) = value.as_object_mut() {
        if let Some(reason) = fallback_reason {
            obj.insert("engine".to_owned(), serde_json::json!(engine.unwrap_or("local-fallback")));
            obj.insert("fallback_reason".to_owned(), serde_json::json!(reason));
            obj.insert(
                "note".to_owned(),
                serde_json::json!("本地兜底稿没有说话人分离和时间戳"),
            );
        }
    }
    Ok(value)
}

/// 保留两位小数（JSON 里别给一串 399.34444444444）。
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// 构造客户端（取 cookie + 组装参数）。
fn build_client(
    section: &TingwuSection,
    store: &dyn ru41::ports::storage::TaskStore,
) -> Result<TingwuClient> {
    let cookie = cookie_from_store(store, section.credential())?;
    TingwuClient::new(&cookie, section)
}

/// 解析 `transcribe` 的参数：文件路径 + 可选 `--format` + 可选 `--text-out`。
///
/// `--text-only` / `--no-fallback` / `--no-quota-check` 是**纯标志**：
/// 这里跳过（dispatch 里再读），别当成未知参数拒掉。
fn parse_transcribe_args(args: &[String]) -> Result<(PathBuf, Format, Option<PathBuf>)> {
    let mut path: Option<PathBuf> = None;
    let mut format = Format::Markdown;
    let mut text_out: Option<PathBuf> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].trim();
        if arg == "--text-only" || arg == "--no-fallback" || arg == "--no-quota-check" {
            // 纯标志：dispatch 里读。
        } else if arg == "--text-out" {
            let value = args
                .get(index + 1)
                .ok_or_else(|| anyhow::anyhow!("--text-out 后面要给文件路径"))?;
            text_out = Some(PathBuf::from(value.trim()));
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--text-out=") {
            text_out = Some(PathBuf::from(value.trim()));
        } else if let Some(value) = arg.strip_prefix("--format=") {
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
    Ok((path, format, text_out))
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
        "speaker_mode": transcript.speaker_mode,
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

// ── 清理 ─────────────────────────────────────────────────────────────

/// `clean` 的参数。
#[derive(Debug, Clone)]
struct CleanOptions {
    /// 要不要动云端任务（默认只清本地）。
    cloud: bool,
    /// 本地：只删 N 天前的（0 = 全删）。
    older_than_days: i64,
    /// 云端：保留最近 N 个（0 = 全删）。
    keep: usize,
    /// 云端：永久删除（不进回收站）。
    permanent: bool,
    /// 真删（不给就只列清单）。
    yes: bool,
    /// 指定要删的任务 ID（给了就只删这些）。
    trans_ids: Vec<String>,
    /// 连**阿里云盘导入**的老转写一起删（默认不碰 —— 那是用户的资料）。
    include_imported: bool,
}

/// 解析 `clean` 的参数。
fn parse_clean_args(args: &[String]) -> Result<CleanOptions> {
    let mut options = CleanOptions {
        cloud: false,
        older_than_days: 7,
        keep: 0,
        permanent: false,
        yes: false,
        trans_ids: Vec::new(),
        include_imported: false,
    };
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].trim();
        match arg {
            "--cloud" => options.cloud = true,
            "--permanent" => options.permanent = true,
            "--yes" | "-y" => options.yes = true,
            "--include-imported" => options.include_imported = true,
            "--older-than-days" | "--keep" | "--trans-id" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| anyhow::anyhow!("{arg} 后面要给值"))?;
                if arg == "--older-than-days" {
                    options.older_than_days = value
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| anyhow::anyhow!("--older-than-days 要一个天数（0 = 全删）"))?;
                } else if arg == "--keep" {
                    options.keep = value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| anyhow::anyhow!("--keep 要一个数字（0 = 全删）"))?;
                } else {
                    options.trans_ids.push(value.trim().to_owned());
                }
                index += 1;
            }
            other if other.starts_with("--") => {
                let (key, value) = other
                    .split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("未知参数 {other}"))?;
                match key {
                    "--older-than-days" => {
                        options.older_than_days = value.trim().parse::<i64>().map_err(|_| {
                            anyhow::anyhow!("--older-than-days 要一个天数（0 = 全删）")
                        })?;
                    }
                    "--keep" => {
                        options.keep = value
                            .trim()
                            .parse::<usize>()
                            .map_err(|_| anyhow::anyhow!("--keep 要一个数字（0 = 全删）"))?;
                    }
                    "--trans-id" => options.trans_ids.push(value.trim().to_owned()),
                    _ => bail!("未知参数 {other}"),
                }
            }
            other => bail!("未知参数 {other}"),
        }
        index += 1;
    }
    Ok(options)
}

/// 本地待删文件（只认插件自己落的稿子：`.md` / `.json` / `.srt` / `.txt`）。
async fn local_candidates(output_dir: &Path, older_than_days: i64) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(output_dir).await else {
        return Ok(out);
    };
    let cutoff = chrono::Local::now() - chrono::Duration::days(older_than_days);
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if !matches!(extension.as_str(), "md" | "json" | "srt" | "txt") {
            continue;
        }
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|delta| chrono::DateTime::from_timestamp_millis(delta.as_millis() as i64))
            .map(|stamp| stamp.with_timezone(&chrono::Local))
            .unwrap_or(cutoff);
        if modified <= cutoff {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// 清理：本地旧稿 + 云端旧任务。
///
/// ⚠️ **默认只列不删**（dry-run）：删东西不可逆，得让调用方显式说 `--yes`。
///
/// ⚠️ **云端只删「本地/网页版上传」的任务**（`task_id` 以 `rc-upload-` 开头）。
/// 听悟账号里还有一大票**阿里云盘导入**的老转写（高考/考研网课那些）——
/// 那是用户的资料，不是垃圾。2026-11-25 实测踩过：按服务端返回顺序删，
/// 把网盘资料一起删了（86 条，事后靠回收站 revertTrashes 全量捞回）。
/// 要连网盘的一起清，得显式 `--include-imported`。
async fn clean(
    client: &TingwuClient,
    output_dir: &Path,
    options: &CleanOptions,
) -> Result<serde_json::Value> {
    let local = local_candidates(output_dir, options.older_than_days).await?;
    let mut cloud: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut skipped_imported: Vec<String> = Vec::new();
    if options.cloud {
        if !options.trans_ids.is_empty() {
            cloud = options.trans_ids.clone();
        } else {
            let mut tasks = client
                .list(100)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            // ⚠️ **不能靠服务端的返回顺序**（实测它把今天的和 2024 年的混着排，
            // 只按位置取「最近 N 个」会连老资料一起选中）—— 自己按 showTime 排。
            tasks.sort_by_key(|task| std::cmp::Reverse(task.show_time_ms));
            for task in &tasks {
                if !task.is_local_upload() && !options.include_imported {
                    skipped_imported.push(format!("{}（{}）", task.show_name, task.task_id));
                    continue;
                }
                if kept.len() < options.keep {
                    kept.push(task.trans_id.clone());
                } else {
                    cloud.push(task.trans_id.clone());
                }
            }
        }
    }

    let mut deleted_local = 0usize;
    let mut deleted_bytes = 0u64;
    if options.yes {
        for path in &local {
            if let Ok(meta) = tokio::fs::metadata(path).await {
                deleted_bytes += meta.len();
            }
            match tokio::fs::remove_file(path).await {
                Ok(()) => deleted_local += 1,
                Err(error) => {
                    eprintln!("[tingwu] 删 {} 失败：{error}", path.display());
                }
            }
        }
        if !cloud.is_empty() {
            client
                .delete_trans(&cloud, options.permanent)
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
        }
    }

    Ok(serde_json::json!({
        "ok": true,
        "dry_run": !options.yes,
        "local_candidates": local.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "local_deleted": deleted_local,
        "local_freed_mib": round2(deleted_bytes as f64 / 1048576.0),
        "cloud_candidates": cloud,
        "cloud_kept": kept,
        "cloud_skipped_imported": skipped_imported,
        "cloud_skipped_imported_count": skipped_imported.len(),
        "cloud_permanent": options.permanent,
        "cloud_deleted": if options.yes { cloud.len() } else { 0 },
        "note": if options.yes {
            "已执行删除（云端任务先进回收站，除非 --permanent）"
        } else {
            "只列清单，没删 —— 真删加 --yes"
        },
    }))
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
        let (path, format, _) = parse_transcribe_args(&args).expect("应解析成功");
        assert_eq!(path, PathBuf::from("/data/a.mp3"));
        assert_eq!(format, Format::Srt);
    }

    #[test]
    fn parses_transcribe_arguments_with_equals() {
        let args = vec!["/data/a.mp3".to_owned(), "--format=text".to_owned()];
        let (_, format, _) = parse_transcribe_args(&args).expect("应解析成功");
        assert_eq!(format, Format::Text);
    }

    #[test]
    fn parses_new_flags_for_other_plugins() {
        // --text-out / --text-only / --no-fallback 是给「别的插件调用」用的，
        // 不能被当成未知参数拒掉。
        let args = vec![
            "/data/a.mp3".to_owned(),
            "--text-only".to_owned(),
            "--no-fallback".to_owned(),
            "--text-out".to_owned(),
            "/tmp/out.txt".to_owned(),
        ];
        let (path, _, text_out) = parse_transcribe_args(&args).expect("应解析成功");
        assert_eq!(path, PathBuf::from("/data/a.mp3"));
        assert_eq!(text_out, Some(PathBuf::from("/tmp/out.txt")));
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

    #[test]
    fn parses_clean_arguments() {
        let args = vec![
            "--cloud".to_owned(),
            "--keep".to_owned(),
            "2".to_owned(),
            "--older-than-days=30".to_owned(),
            "--permanent".to_owned(),
            "--yes".to_owned(),
        ];
        let options = parse_clean_args(&args).expect("应解析成功");
        assert!(options.cloud);
        assert!(options.permanent);
        assert!(options.yes);
        assert_eq!(options.keep, 2);
        assert_eq!(options.older_than_days, 30);
    }

    #[test]
    fn clean_defaults_to_dry_run() {
        let options = parse_clean_args(&[]).expect("应解析成功");
        assert!(!options.yes, "不给 --yes 就必须只列不删");
        assert!(!options.cloud, "默认不动云端");
        assert_eq!(options.older_than_days, 7);
        assert!(!options.include_imported, "默认不许碰阿里云盘导入的老资料");
    }

    #[test]
    fn clean_can_opt_into_imported() {
        let options = parse_clean_args(&["--include-imported".to_owned()]).expect("应解析成功");
        assert!(options.include_imported);
    }

    #[test]
    fn clean_rejects_unknown_arguments() {
        assert!(parse_clean_args(&["--nope".to_owned()]).is_err());
        assert!(parse_clean_args(&["--keep".to_owned()]).is_err());
    }

    #[test]
    fn round2_keeps_two_decimals() {
        assert_eq!(round2(399.34444), 399.34);
        assert_eq!(round2(1.005), 1.0);
    }
}
