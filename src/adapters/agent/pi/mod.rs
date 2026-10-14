//! Pi 1.2 RPC 分析适配器。模型与认证由 Pi 管理，宿主只管理进程和结果契约。
use crate::{
    domain::{AgentResult, RunContract, error::Ru41Error},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// 仅提供无工具分析；不声明审批或会话恢复能力。
#[derive(Clone, Debug)]
pub struct PiAgent {
    pub executable: PathBuf,
    pub model: Option<String>,
    pub provider: Option<String>,
}
impl Default for PiAgent {
    fn default() -> Self {
        Self {
            executable: "pi".into(),
            model: None,
            provider: None,
        }
    }
}
impl AgentBackend for PiAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "pi".into(),
            model: self.model.clone(),
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: false,
        }
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + '_>> {
        Box::pin(self.execute(request))
    }
}
impl PiAgent {
    async fn execute(&self, request: AgentRequest) -> Result<AgentOutput> {
        ensure!(
            request.contract == RunContract::AnalysisV1,
            Ru41Error::UnsupportedContract(request.contract.as_str().into())
        );
        ensure!(
            self.provider.is_none() || self.model.is_some(),
            Ru41Error::Config("Pi provider requires model".into())
        );
        let mut command = tokio::process::Command::new(&self.executable);
        command.current_dir(&request.run_dir).args([
            "--mode", "rpc", "--no-session", "--no-tools", "--no-extensions",
            "--no-mcp", "--no-skills", "--no-prompt-templates", "--no-themes",
            "--no-context-files", "--no-approve", "--offline",
            "--system-prompt", "Analyze only the supplied request. Return exactly a JSON object with one nonempty string field named summary. Do not claim to have performed external actions.",
        ]).env("PI_TELEMETRY", "0")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(model) = &self.model {
            command.args(["--model", model]);
        }
        if let Some(provider) = &self.provider {
            command.args(["--provider", provider]);
        }
        #[cfg(unix)]
        command.process_group(0);
        let child = command.spawn().map_err(|e| {
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) {
                anyhow::Error::new(Ru41Error::Config(format!("cannot start Pi: {e}")))
            } else {
                anyhow::Error::new(e)
            }
        })?;
        let mut owned = OwnedChild(Some(child));
        let child = owned.0.as_mut().expect("owned child");
        let _group = ProcessGroup(child.id());
        let mut stdin = child.stdin.take().context("Pi stdin missing")?;
        let stdout = child.stdout.take().context("Pi stdout missing")?;
        let mut stderr = child.stderr.take().context("Pi stderr missing")?;
        let protocol = async {
            stdin
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id":"ru41-prompt","type":"prompt","message":request.prompt})
                    )
                    .as_bytes(),
                )
                .await?;
            stdin.flush().await?;
            let mut reader = BufReader::new(stdout);
            let mut final_message: Option<Value> = None;
            let mut accepted = false;
            let mut settled = false;
            let mut total = 0_usize;
            loop {
                let mut line = Vec::new();
                let n = (&mut reader)
                    .take(1_048_577)
                    .read_until(b'\n', &mut line)
                    .await?;
                ensure!(
                    n > 0,
                    Ru41Error::AgentProcess("Pi exited before completion".into())
                );
                total += n;
                ensure!(
                    n <= 1_048_576 && total <= 16 * 1_048_576,
                    Ru41Error::AgentProcess("Pi protocol output exceeded limit".into())
                );
                let event: Value = serde_json::from_slice(&line)
                    .map_err(|_| Ru41Error::AgentProcess("invalid Pi JSONL record".into()))?;
                match event["type"].as_str() {
                    Some("response") if event["id"] == "ru41-prompt" => {
                        ensure!(
                            event["success"] == true,
                            Ru41Error::AgentProcess("Pi rejected prompt".into())
                        );
                        ensure!(
                            event["data"]["disposition"] != "handled",
                            Ru41Error::InvalidOutput(
                                "Pi handled prompt without an agent run".into()
                            )
                        );
                        accepted = true;
                    }
                    Some("message_end") if event["message"]["role"] == "assistant" => {
                        final_message = Some(event["message"].clone());
                    }
                    Some("agent_settled") => {
                        ensure!(
                            event["aborted"] == false,
                            Ru41Error::AgentProcess("Pi run aborted".into())
                        );
                        settled = true;
                    }
                    Some("extension_ui_request") => {
                        return Err(Ru41Error::UnsupportedContract(
                            "Pi extension interaction disabled in analysis mode".into(),
                        )
                        .into());
                    }
                    _ => {}
                }
                if accepted && settled {
                    let message =
                        final_message.context("Pi did not produce an assistant message")?;
                    ensure!(
                        message["stopReason"] == "stop",
                        Ru41Error::AgentProcess("Pi assistant did not finish normally".into())
                    );
                    let mut text = String::new();
                    for block in message["content"]
                        .as_array()
                        .context("Pi content missing")?
                    {
                        if block["type"] == "text" {
                            text.push_str(block["text"].as_str().context("Pi text missing")?);
                        }
                    }
                    let result: AgentResult = serde_json::from_str(&text).map_err(|_| {
                        Ru41Error::InvalidOutput("Pi result must be JSON with summary".into())
                    })?;
                    result.validate().map_err(Ru41Error::InvalidOutput)?;
                    stdin.shutdown().await?;
                    return Ok::<_, anyhow::Error>(AgentOutput {
                        result,
                        session_id: None,
                    });
                }
            }
        };
        let diagnostics = async {
            let mut count = 0;
            let mut buf = [0_u8; 8192];
            loop {
                let n = stderr.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                count += n;
                ensure!(
                    count <= 2 * 1_048_576,
                    Ru41Error::AgentProcess("Pi stderr exceeded limit".into())
                );
            }
            std::future::pending::<Result<()>>().await
        };
        tokio::pin!(diagnostics);
        let outcome = tokio::select! {
            result = protocol => result,
            result = &mut diagnostics => {
                result?;
                Err(Ru41Error::AgentProcess("Pi stderr closed before completion".into()).into())
            }
        };
        // 无会话续跑；完成和取消均清理子进程，避免宿主留下常驻 Pi。
        let _ = child.kill().await;
        let _ = child.wait().await;
        outcome
    }
}
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: pid 来自本次新建进程组，不涉及指针。
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

// Future 被取消时仍回收直接子进程，避免僵尸累积。
struct OwnedChild(Option<tokio::process::Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}
