use anyhow::{Context, Result, bail};
use std::process::Command;

/// 只检查已安装的 CLI，不做认证也不调用模型。
pub fn version() -> Result<String> {
    let output = Command::new("codex")
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

use crate::{
    app::agent_service::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor},
    domain::{AgentResult, RunContract},
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
}

impl Default for CodexAgent {
    fn default() -> Self {
        Self {
            executable: "codex".into(),
            model: None,
        }
    }
}

impl AgentBackend for CodexAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex".into(),
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

impl CodexAgent {
    async fn execute(&self, request: AgentRequest) -> Result<AgentOutput> {
        anyhow::ensure!(
            self.descriptor().contracts.contains(&request.contract),
            "unsupported Codex run contract"
        );
        let schema = request.run_dir.join("result.schema.json");
        let output_file = request.run_dir.join("result.json");
        tokio::fs::write(&schema, r#"{"type":"object","properties":{"summary":{"type":"string"}},"required":["summary"],"additionalProperties":false}"#).await?;
        let mut cmd = tokio::process::Command::new(&self.executable);
        cmd.current_dir(&request.run_dir)
            .args([
                "exec",
                "--json",
                "--ephemeral",
                "--ignore-user-config",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--color",
                "never",
            ])
            .arg("--output-schema")
            .arg(&schema)
            .arg("--output-last-message")
            .arg(&output_file)
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(model) = &self.model {
            cmd.arg("--model").arg(model);
        }
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
        let prompt = format!(
            "You are Ru41's analysis agent. Use only the supplied request. Return a JSON object with a nonempty summary. Do not modify files, submit assignments, publish, or claim any external action was completed. If the request requires an external action, explain the proposed steps and the limitation.\n\nRequest:\n{}",
            request.prompt
        );
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
        anyhow::ensure!(
            status.success(),
            "Codex exited with {status}; inspect the attempt stderr.log"
        );
        let mut thread_id = None;
        let mut completed = false;
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
                Some("turn.failed" | "error") => {
                    bail!("Codex reported an error; inspect the attempt events.jsonl")
                }
                _ => {}
            }
        }
        anyhow::ensure!(completed, "Codex did not report turn.completed");
        // 即使子进程产出畸形的超大文件，也做有界读取。
        let file = tokio::fs::File::open(&output_file)
            .await
            .context("Codex did not create a final result")?;
        let mut bytes = Vec::new();
        file.take(131_073).read_to_end(&mut bytes).await?;
        anyhow::ensure!(bytes.len() <= 131_072, "Codex result exceeds size limit");
        let result: AgentResult =
            serde_json::from_slice(&bytes).context("invalid Codex result JSON")?;
        result.validate().map_err(anyhow::Error::msg)?;
        Ok(AgentOutput {
            result,
            session_id: thread_id,
        })
    }
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

struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: pid 是子进程创建的进程组；不涉及任何指针。
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}
