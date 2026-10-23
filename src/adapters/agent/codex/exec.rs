use super::super::process::ProcessGroup;
use anyhow::{Context, Result, bail};
use std::process::Command;

/// 只检查已安装的 CLI，不做认证也不调用模型。
pub fn version() -> Result<String> {
    version_with(std::path::Path::new("codex"))
}

pub fn version_with(executable: &std::path::Path) -> Result<String> {
    let output = Command::new(executable)
        .arg("--version")
        .output()
        .context("cannot start Codex CLI; install it and make sure codex is on PATH")?;
    if !output.status.success() {
        bail!("codex --version failed with {}", output.status);
    }
    Ok(String::from_utf8(output.stdout)
        .context("Codex version output was not UTF-8")?
        .trim()
        .to_owned())
}

/// 底座 CLI 的可选能力：新版 codex（≥0.16x）移除了 wire_api="chat" 支持，
/// 需要用旧版（如 0.80）对接只提供 chat 兼容端点的供应商（智谱 GLM coding plan）。
/// 旧版同时缺少 --ephemeral / --ignore-user-config，且 resume 子命令不支持
/// --output-last-message，按探测结果条件传参、并从事件流提取最终消息。
#[derive(Clone, Copy)]
struct CliCaps {
    ephemeral: bool,
    ignore_user_config: bool,
    resume_output_last_message: bool,
}

impl Default for CliCaps {
    fn default() -> Self {
        Self {
            ephemeral: false,
            ignore_user_config: false,
            // 探测失败时假定现代版本，保持既有 resume 行为。
            resume_output_last_message: true,
        }
    }
}

fn help_text(executable: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(executable)
        .args(args)
        // 探测不得继承 stdin（假脚本可能 cat 等输入），也不得污染调用方目录。
        .stdin(std::process::Stdio::null())
        .current_dir("/")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn cli_caps(executable: &Path) -> CliCaps {
    static CACHE: std::sync::Mutex<Option<std::collections::BTreeMap<String, CliCaps>>> =
        std::sync::Mutex::new(None);
    let key = executable.to_string_lossy().into_owned();
    {
        let guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(caps) = guard.as_ref().and_then(|map| map.get(&key)) {
            return *caps;
        }
    }
    let caps = match (
        help_text(executable, &["exec", "--help"]),
        help_text(executable, &["exec", "resume", "--help"]),
    ) {
        (Some(exec_help), Some(resume_help)) => CliCaps {
            ephemeral: exec_help.contains("--ephemeral"),
            ignore_user_config: exec_help.contains("--ignore-user-config"),
            resume_output_last_message: resume_help.contains("--output-last-message"),
        },
        _ => CliCaps::default(),
    };
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(Default::default)
        .insert(key, caps);
    caps
}

use crate::{
    domain::{AgentResult, RunContract, error::Ru41Error},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor},
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub struct CodexAgent {
    pub executable: PathBuf,
    pub model: Option<String>,
    /// 附加的 `-c key=value` 覆盖项（自定义 provider 等）。
    pub config_overrides: Vec<String>,
    pub output_schema: bool,
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self {
            executable: "codex".into(),
            model: None,
            config_overrides: Vec::new(),
            output_schema: true,
        }
    }
}

impl AgentBackend for CodexAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex".into(),
            model: self.model.clone(),
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: true,
        }
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + '_>> {
        Box::pin(self.execute(request))
    }
}

impl CodexAgent {
    async fn execute(&self, request: AgentRequest) -> Result<AgentOutput> {
        if !self.descriptor().contracts.contains(&request.contract) {
            return Err(
                Ru41Error::UnsupportedContract(request.contract.as_str().to_string()).into(),
            );
        }
        let schema = request.run_dir.join("result.schema.json");
        let output_file = request.run_dir.join("result.json");
        let skills = request
            .context
            .as_ref()
            .map_or(&[][..], |context| context.skills.as_slice());
        crate::skills::materialize(&request.run_dir, skills)?;
        let (persistent, resume_session_id) =
            request.context.as_ref().map_or((false, None), |context| {
                (
                    context.conversation_id.is_some(),
                    context.resume_session_id.clone(),
                )
            });
        let resuming = resume_session_id.is_some();
        let caps = cli_caps(&self.executable);
        // 旧版 resume 子命令不支持 --output-last-message：改从事件流提取最终消息。
        let use_output_file = !resuming || caps.resume_output_last_message;
        if use_output_file && self.output_schema {
            tokio::fs::write(&schema, r#"{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"],"additionalProperties":false}"#).await?;
        }
        let mut cmd = tokio::process::Command::new(&self.executable);
        cmd.current_dir(&request.run_dir).arg("exec");
        if let Some(session_id) = resume_session_id {
            cmd.arg("resume").arg(session_id);
        }
        cmd.args([
            "--json",
            "--skip-git-repo-check",
            "--dangerously-bypass-approvals-and-sandbox",
        ]);
        if caps.ignore_user_config {
            cmd.arg("--ignore-user-config");
        }
        if !resuming {
            cmd.args(["--color", "never"]);
        }
        // 旧版没有 --ephemeral：默认就会持久化会话，续接行为不受影响。
        if !persistent && caps.ephemeral {
            cmd.arg("--ephemeral");
        }
        if use_output_file {
            cmd.arg("--output-last-message").arg(&output_file);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if use_output_file && self.output_schema {
            cmd.arg("--output-schema").arg(&schema);
        }
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
        for override_ in &self.config_overrides {
            cmd.arg("-c").arg(override_);
        }
        // `-` 是 stdin 提示词的位置参数，必须排在全部选项之后。
        cmd.arg("-");
        #[cfg(unix)]
        {
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("cannot start Codex CLI")?;
        // 独立进程组也便于取消时清理子孙进程。
        let _group = ProcessGroup(child.id());
        let mut stdin = child.stdin.take().context("missing Codex stdin")?;
        let stdout = child.stdout.take().context("missing Codex stdout")?;
        let stderr = child.stderr.take().context("missing Codex stderr")?;
        let prompt = request.prompt;
        let events_path = request.run_dir.join("events.jsonl");
        let stderr_path = request.run_dir.join("stderr.log");
        let ((), status, events, _) = tokio::try_join!(
            async {
                stdin.write_all(prompt.as_bytes()).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<_, anyhow::Error>(())
            },
            async { Ok::<_, anyhow::Error>(child.wait().await?) },
            capture(stdout, &events_path),
            capture(stderr, &stderr_path),
        )?;
        if !status.success() {
            return Err(Ru41Error::AgentProcess(format!(
                "Codex exited with {status}{}",
                last_event_error(&events)
            ))
            .into());
        }
        let mut thread_id = None;
        let mut completed = false;
        let mut error_reported = false;
        for line in events.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let event: serde_json::Value =
                serde_json::from_slice(line).context("invalid Codex JSONL event")?;
            match event.get("type").and_then(|v| v.as_str()) {
                Some("thread.started") => {
                    thread_id = event
                        .get("thread_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                }
                Some("turn.completed") => completed = true,
                Some("turn.failed") => {
                    return Err(Ru41Error::AgentProcess(format!(
                        "Codex turn failed{}",
                        last_event_error(&events)
                    ))
                    .into());
                }
                Some("error") => error_reported = true,
                _ => {}
            }
        }
        if !completed {
            let message = if error_reported {
                format!("Codex reported an error{}", last_event_error(&events))
            } else {
                "Codex did not report turn.completed".to_owned()
            };
            return Err(Ru41Error::AgentProcess(message.into()).into());
        }
        let result: AgentResult = if use_output_file {
            // 即使子进程产出畸形的超大文件，也做有界读取。
            let file = tokio::fs::File::open(&output_file)
                .await
                .context("Codex did not create a final result")?;
            let mut bytes = Vec::new();
            file.take(131_073).read_to_end(&mut bytes).await?;
            anyhow::ensure!(bytes.len() <= 131_072, "Codex result exceeds size limit");
            if self.output_schema {
                serde_json::from_slice(&bytes).map_err(|e| {
                    Ru41Error::InvalidOutput(format!("invalid Codex result JSON: {e}"))
                })?
            } else {
                AgentResult {
                    summary: String::from_utf8(bytes)
                        .map_err(|e| {
                            Ru41Error::InvalidOutput(format!("invalid Codex result UTF-8: {e}"))
                        })?
                        .trim()
                        .to_owned(),
                }
            }
        } else {
            AgentResult {
                summary: final_message_from_events(&events).ok_or_else(|| {
                    Ru41Error::InvalidOutput(
                        "resumed Codex run produced no agent message in events".into(),
                    )
                })?,
            }
        };
        super::ensure_tool_calls_executed(&result.summary)?;
        result
            .validate()
            .map_err(|e| anyhow::Error::new(Ru41Error::InvalidOutput(e)))?;
        Ok(AgentOutput {
            result,
            session_id: thread_id,
        })
    }
}

/// 从事件字节里提取最后一条 agent_message（旧版 resume 没有 --output-last-message
/// 时的最终消息来源）。
fn final_message_from_events(events: &[u8]) -> Option<String> {
    let mut last = None;
    for line in events.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if event.get("type").and_then(|v| v.as_str()) != Some("item.completed") {
            continue;
        }
        let item = event.get("item")?;
        if item.get("type").and_then(|v| v.as_str()) == Some("agent_message")
            && let Some(text) = item.get("text").and_then(|v| v.as_str())
        {
            last = Some(text.trim().to_owned());
        }
    }
    last.filter(|text| !text.is_empty())
}

/// 从事件字节里提取最有信息量的底座错误（turn.failed 优先，其次最后一条 error），
/// 让任务失败原因直接可读（如 provider 402 余额不足），而不是让用户去翻日志。
fn last_event_error(events: &[u8]) -> String {
    let mut last_error = None;
    let mut turn_failed = None;
    for line in events.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        match event.get("type").and_then(|v| v.as_str()) {
            Some("error") => {
                if let Some(message) = event.get("message").and_then(|v| v.as_str()) {
                    last_error = Some(message.to_owned());
                }
            }
            Some("turn.failed") => {
                if let Some(message) = event
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                {
                    turn_failed = Some(message.to_owned());
                }
            }
            _ => {}
        }
    }
    let message = turn_failed.or(last_error);
    message.map_or(String::new(), |text| {
        let mut text = text.trim().to_owned();
        text.truncate(240);
        format!(": {text}")
    })
}

/// 并发地排空两个流；日志超过上限则停止运行。
async fn capture(mut reader: impl AsyncRead + Unpin, path: &Path) -> Result<Vec<u8>> {
    let mut file = tokio::fs::File::create(path).await?;
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let count = reader.read(&mut buf).await?;
        if count == 0 {
            break;
        }
        anyhow::ensure!(
            data.len() + count <= 2 * 1024 * 1024,
            "Codex log exceeds 2 MiB limit"
        );
        file.write_all(&buf[..count]).await?;
        data.extend_from_slice(&buf[..count]);
    }
    file.flush().await?;
    Ok(data)
}
