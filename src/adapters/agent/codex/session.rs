use super::super::process::ProcessGroup;
use crate::{
    domain::{AgentResult, RunContract, error::Ru41Error, records::Interaction},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor},
    runtime::worker::now,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{future::Future, path::PathBuf, pin::Pin, process::Stdio};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct CodexSession {
    pub executable: PathBuf,
    pub model: Option<String>,
    /// 附加的 `-c key=value` 覆盖项（自定义 provider 等）。
    pub config_overrides: Vec<String>,
}
impl AgentBackend for CodexSession {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex-session".into(),
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
async fn send(writer: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    writer.write_all(format!("{value}\n").as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}
impl CodexSession {
    async fn execute(&self, request: AgentRequest) -> Result<AgentOutput> {
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(&request.run_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for override_ in &self.config_overrides {
            command.arg("-c").arg(override_);
        }
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().context("cannot start Codex app-server")?;
        let _guard = ProcessGroup(child.id());
        let mut writer = child.stdin.take().context("missing stdin")?;
        let mut reader = BufReader::new(child.stdout.take().context("missing stdout")?).lines();
        send(&mut writer,json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"ru41","version":"0.3.0"},"capabilities":{"experimentalApi":false}}})).await?;
        let mut session = None;
        let mut final_text = String::new();
        while let Some(line) = reader.next_line().await? {
            ensure!(line.len() <= 2 * 1024 * 1024, "oversized app-server frame");
            let frame: Value = serde_json::from_str(&line)?;
            if frame.get("error").is_some() {
                return Err(
                    Ru41Error::AgentProcess("Codex app-server request failed".into()).into(),
                );
            }
            match frame.get("id").and_then(Value::as_u64) {
                Some(1) if frame.get("method").is_none() => {
                    send(&mut writer, json!({"method":"initialized"})).await?;
                    send(&mut writer,json!({"id":2,"method":"thread/start","params":{"model":self.model,"cwd":request.run_dir,"approvalPolicy":"on-request","sandbox":"read-only","ephemeral":true}})).await?;
                }
                Some(2) if frame.get("method").is_none() => {
                    let id = frame["result"]["thread"]["id"]
                        .as_str()
                        .context("missing thread id")?
                        .to_string();
                    send(&mut writer,json!({"id":3,"method":"turn/start","params":{"threadId":id,"input":[{"type":"text","text":request.prompt}],"outputSchema":{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"],"additionalProperties":false}}})).await?;
                    session = Some(id);
                }
                _ => {}
            }
            let method = frame["method"].as_str().unwrap_or("");
            if frame.get("id").is_some() && !method.is_empty() {
                let options = match method {
                    "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                        vec![
                            json!({"decision":"accept"}),
                            json!({"decision":"decline"}),
                            json!({"decision":"cancel"}),
                        ]
                    }
                    _ => {
                        send(&mut writer,json!({"id":frame["id"],"error":{"code":-32601,"message":"unsupported interaction"}})).await?;
                        continue;
                    }
                };
                let context = request
                    .context
                    .as_ref()
                    .context("interactive backend needs host context")?;
                let result = context
                    .interactions
                    .request(Interaction {
                        id: format!("{}-{}", context.run_id, rand::random::<u64>()),
                        run_id: context.run_id.clone(),
                        request_id: frame["id"].clone(),
                        method: method.into(),
                        detail: frame["params"].clone(),
                        options,
                        expires_at: now() + 300,
                        status: "pending".into(),
                        response: None,
                    })
                    .await?;
                send(&mut writer, json!({"id":frame["id"],"result":result})).await?;
            }
            if method == "item/completed" && frame["params"]["item"]["type"] == "agentMessage" {
                final_text = frame["params"]["item"]["text"]
                    .as_str()
                    .unwrap_or("")
                    .into();
            }
            if method == "turn/completed" {
                ensure!(
                    frame["params"]["turn"]["status"] == "completed",
                    "Codex turn did not complete"
                );
                let result: AgentResult =
                    serde_json::from_str(&final_text).context("invalid structured agent result")?;
                result.validate().map_err(anyhow::Error::msg)?;
                child.kill().await.ok();
                let _ = child.wait().await;
                return Ok(AgentOutput {
                    result,
                    session_id: session,
                });
            }
        }
        Err(Ru41Error::AgentProcess("Codex app-server ended before turn completion".into()).into())
    }
}
