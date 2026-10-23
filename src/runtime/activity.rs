//! 把底座（Codex）的执行过程实时翻译成 QQ 可读的进度消息。
//!
//! 底座以 `--json` 运行时把每个事件写入 run_dir/events.jsonl（见
//! adapters/agent/codex/exec.rs 的 capture）。本模块尾随该文件，把事件
//! 映射为简短的展示行（💬/💭/⚙️/🔍），按节流窗口攒批写入 `activity`
//! 文档（每个 chunk 一个文档，id 内含零填充序号保证 dispatcher 按序发送）。
//! Worker 与 QQ 网关是独立进程，store 就是两者的消息总线。

use crate::ports::storage::TaskStore;
use anyhow::Result;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

/// 单个 run 的活动流推送器；run_claim 在 await 底座 future 的同时 spawn 它。
pub struct ActivityStreamer {
    pub store: Arc<dyn TaskStore>,
    pub run_id: String,
    pub task_id: i64,
    pub attempt_id: i64,
    /// 底座事件文件（codex 为 run_dir/events.jsonl）。
    pub path: PathBuf,
    /// 攒批 flush 的节流间隔。
    pub interval: Duration,
    /// 单次尝试最多推送的 chunk 数；超出后静默等待终态通知。
    pub max_chunks: u32,
}

impl ActivityStreamer {
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        match self.run_inner(&mut stop).await {
            Ok(chunks) => {
                if chunks > 0 {
                    eprintln!("[activity] {run} streamed {chunks} chunks", run = self.run_id);
                }
            }
            Err(error) => {
                eprintln!("[activity] {run} streamer error: {error:#}", run = self.run_id)
            }
        }
    }

    async fn run_inner(&self, stop: &mut watch::Receiver<bool>) -> Result<usize> {
        // codex 进程启动到吐出首个事件可能需要几秒；最多等 60 秒。
        let mut waited = 0_u32;
        while !self.path.exists() {
            if *stop.borrow() {
                return Ok(0);
            }
            waited += 1;
            if waited > 60 {
                return Ok(0);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let mut file = tokio::fs::File::open(&self.path).await?;
        let mut carry: Vec<u8> = Vec::new();
        let mut pending: Vec<String> = Vec::new();
        let mut seq = 0_u32;
        let mut last_flush = std::time::Instant::now();
        loop {
            let stopped = *stop.borrow();
            // 尽量排空本轮可读的字节；文件末尾 read 返回 0。
            let mut buf = [0u8; 65536];
            let read = file.read(&mut buf).await.unwrap_or(0);
            if read > 0 {
                carry.extend_from_slice(&buf[..read]);
                while let Some(pos) = carry.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = carry.drain(..=pos).collect();
                    let line = &line[..line.len() - 1];
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) else {
                        continue;
                    };
                    if let Some(display) = display_line(&event) {
                        pending.push(display);
                    }
                }
            }
            let due = last_flush.elapsed() >= self.interval;
            if stopped || due {
                if !pending.is_empty() && seq < self.max_chunks {
                    self.write_chunk(seq, &pending)?;
                    seq += 1;
                    pending.clear();
                    last_flush = std::time::Instant::now();
                }
                if stopped {
                    return Ok(seq as usize);
                }
                if seq >= self.max_chunks {
                    // 达到上限：告知用户后续不再逐条推送，避免无声黑盒。
                    let _ = self.write_chunk(
                        seq,
                        &["⏸ 执行过程较长，后续步骤不再逐条推送，完成后通知你。".to_owned()],
                    );
                    return Ok(seq as usize + 1);
                }
            }
            if read == 0 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    }

    fn write_chunk(&self, seq: u32, lines: &[String]) -> Result<bool> {
        self.store.put_document(
            "activity",
            &format!("{}-a{seq:03}", self.run_id),
            None,
            &serde_json::json!({
                "task_id": self.task_id,
                "attempt_id": self.attempt_id,
                "run_id": self.run_id,
                "seq": seq,
                "lines": lines,
                "status": "pending",
            }),
        )
    }
}

/// 把一条 codex JSONL 事件映射为展示行；无关事件返回 None。
/// 只取 item.completed（item.started 会成对重复）。
pub fn display_line(event: &serde_json::Value) -> Option<String> {
    match event.get("type").and_then(|v| v.as_str()) {
        Some("item.completed") => {
            let item = event.get("item")?;
            match item.get("type").and_then(|v| v.as_str()) {
                Some("agent_message") => {
                    let text = item.get("text").and_then(|v| v.as_str())?.trim();
                    (!text.is_empty()).then(|| format!("💬 {}", truncate(text, 120)))
                }
                Some("reasoning") => {
                    let text = item
                        .get("summary")
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())?
                        .trim();
                    (!text.is_empty()).then(|| format!("💭 {}", truncate(text, 120)))
                }
                Some("function_call") => {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                    let args = item
                        .get("arguments")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .unwrap_or("");
                    Some(format!("⚙️ {name}{}", truncate_pre(" ", args, 90)))
                }
                Some("local_shell_call") => {
                    let action = item.get("action");
                    let command = action
                        .and_then(|a| a.get("command"))
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("command").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .trim();
                    Some(format!("⚙️ $ {}", truncate(command, 100)))
                }
                Some("web_search_call") => {
                    let query = item
                        .get("action")
                        .and_then(|a| a.get("query"))
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("query").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .trim();
                    Some(format!("🔍 {}", truncate(query, 100)))
                }
                Some("plan_update") => Some("📋 更新了计划".to_owned()),
                _ => None,
            }
        }
        _ => None,
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 有内容才带前缀截断（工具参数为空时不留尾巴空格）。
fn truncate_pre(prefix: &str, text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("{prefix}{}", truncate(text, max_chars))
    }
}

#[cfg(test)]
mod tests {
    use super::display_line;

    fn event(json: &str) -> serde_json::Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn maps_agent_message() {
        let line = display_line(&event(
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"我先看看目录"}}"#,
        ));
        assert_eq!(line.as_deref(), Some("💬 我先看看目录"));
    }

    #[test]
    fn maps_reasoning_summary_and_truncates() {
        let long = "x".repeat(200);
        let line = display_line(&event(&format!(
            r#"{{"type":"item.completed","item":{{"type":"reasoning","summary":"{long}"}}}}"#
        )));
        let line = line.unwrap();
        assert!(line.starts_with("💭 "));
        assert!(line.ends_with('…'));
        assert!(line.chars().count() <= 124);
    }

    #[test]
    fn maps_function_call_with_args() {
        let line = display_line(&event(
            r#"{"type":"item.completed","item":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"ls -la\"}"}}"#,
        ));
        assert_eq!(
            line.as_deref(),
            Some("⚙️ exec_command {\"cmd\":\"ls -la\"}")
        );
    }

    #[test]
    fn maps_shell_and_search_and_plan() {
        let shell = display_line(&event(
            r#"{"type":"item.completed","item":{"type":"local_shell_call","action":{"command":"df -h"}}}"#,
        ));
        assert_eq!(shell.as_deref(), Some("⚙️ $ df -h"));
        let search = display_line(&event(
            r#"{"type":"item.completed","item":{"type":"web_search_call","action":{"query":"rust tokio"}}}"#,
        ));
        assert_eq!(search.as_deref(), Some("🔍 rust tokio"));
        let plan = display_line(&event(
            r#"{"type":"item.completed","item":{"type":"plan_update"}}"#,
        ));
        assert_eq!(plan.as_deref(), Some("📋 更新了计划"));
    }

    #[test]
    fn ignores_started_pairs_and_other_events() {
        assert!(display_line(&event(
            r#"{"type":"item.started","item":{"type":"agent_message","text":"重复"}}"#
        ))
        .is_none());
        assert!(display_line(&event(r#"{"type":"turn.completed"}"#)).is_none());
        assert!(display_line(&event(r#"{"type":"thread.started"}"#)).is_none());
    }
}
