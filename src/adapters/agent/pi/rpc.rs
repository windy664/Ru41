use super::super::process::{OwnedChild, ProcessGroup};
// Pi 1.2 RPC 分析适配器。模型与认证由 Pi 管理，宿主只管理进程和结果契约。
use crate::{
    domain::{AgentResult, RunContract, error::Ru41Error},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor, TokenUsage},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Pi 支持的兼容端点协议。
const SUPPORTED_APIS: [&str; 3] = [
    "anthropic-messages",
    "openai-completions",
    "openai-responses",
];

/// 分析底座；`session` 开启时持久化会话并支持按 session_id 恢复。
#[derive(Clone, Debug)]
pub struct PiAgent {
    pub executable: PathBuf,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// 自定义端点；写入临时 agent 目录的 models.json，由 Pi 识别兼容端点。
    pub base_url: Option<String>,
    /// 端点协议；缺省按 anthropic-messages 处理。
    pub api: Option<String>,
    /// 密钥来源的环境变量名：值只进子进程环境，models.json 里只写 `$NAME`。
    pub api_key_env: Option<String>,
    /// 会话持久化开关。
    pub session: bool,
    /// 会话存储目录。
    pub session_dir: Option<PathBuf>,
    /// 工具白名单（Pi 的 --tools）；为空则禁用全部工具，只做文本分析。
    pub tools: Option<String>,
    /// 底座 ID（pi / pi-session）。
    pub id: String,
}
impl Default for PiAgent {
    fn default() -> Self {
        Self {
            executable: "pi".into(),
            model: None,
            provider: None,
            base_url: None,
            api: None,
            api_key_env: None,
            session: false,
            session_dir: None,
            tools: None,
            id: "pi".into(),
        }
    }
}
impl AgentBackend for PiAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: self.id.clone(),
            model: self.model.clone(),
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: self.session,
        }
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOutput>> + Send + '_>> {
        Box::pin(self.execute(request))
    }
}

/// Pi 上报的用量字段与 Codex 命名不同（input/output/cacheRead/totalTokens），
/// 这里折算成统一的 TokenUsage；累计口径由 Pi 给出，直接取最新一次上报。
fn usage_from_event(event: &Value) -> Option<TokenUsage> {
    let usage = event.get("usage")?;
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    let parsed = TokenUsage {
        input_tokens: field("input"),
        cached_input_tokens: field("cacheRead"),
        output_tokens: field("output"),
        total_tokens: field("totalTokens"),
    };
    (parsed.total_tokens > 0 || parsed.input_tokens > 0 || parsed.output_tokens > 0)
        .then_some(parsed)
}

/// 从 get_state 响应里取会话 ID（不同版本字段名不一，逐个兜底）。
fn session_id_from_state(data: &Value) -> Option<String> {
    for key in ["sessionId", "session_id", "id"] {
        if let Some(value) = data.get(key).and_then(Value::as_str)
            && !value.trim().is_empty()
        {
            return Some(value.to_owned());
        }
    }
    data.get("session")
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

impl PiAgent {
    /// 兼容端点配置：models.json 内容；密钥以 `$ENV` 引用写入，真实值只留在进程环境里。
    pub fn models_json(&self) -> Result<Value> {
        let api = self.api.as_deref().unwrap_or("anthropic-messages");
        ensure!(
            SUPPORTED_APIS.contains(&api),
            Ru41Error::Config(format!(
                "pi api must be one of {} (got {api})",
                SUPPORTED_APIS.join(", ")
            ))
        );
        let provider_id = self.provider.clone().unwrap_or_else(|| "ru41".into());
        let model_id = self
            .model
            .as_deref()
            .map(|m| m.rsplit_once('/').map_or(m, |(_, id)| id))
            .unwrap_or_default();
        let mut provider = serde_json::Map::new();
        provider.insert(
            "baseUrl".into(),
            Value::String(self.base_url.clone().unwrap_or_default()),
        );
        provider.insert("api".into(), Value::String(api.to_owned()));
        if let Some(env_name) = self.api_key_env.as_deref() {
            provider.insert("apiKey".into(), Value::String(format!("${env_name}")));
        }
        provider.insert("models".into(), json!([{ "id": model_id }]));
        let mut root = serde_json::Map::new();
        root.insert(
            "providers".into(),
            json!({ provider_id: Value::Object(provider) }),
        );
        Ok(Value::Object(root))
    }

    /// 把 models.json 写到运行目录下的临时 agent 目录，并返回该目录。
    fn write_models_json(&self, run_dir: &Path) -> Result<PathBuf> {
        let root = self.models_json()?;
        let agent_dir = run_dir.join(".pi-agent");
        std::fs::create_dir_all(&agent_dir)
            .with_context(|| format!("创建 Pi agent 目录失败 {}", agent_dir.display()))?;
        let models_path = agent_dir.join("models.json");
        std::fs::write(
            &models_path,
            serde_json::to_vec_pretty(&root).context("序列化 models.json 失败")?,
        )
        .with_context(|| format!("写 models.json 失败 {}", models_path.display()))?;
        Ok(agent_dir)
    }
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
        command
            .current_dir(&request.run_dir)
            .args([
                "--mode",
                "rpc",
                "--no-extensions",
                "--no-mcp",
                "--no-skills",
                "--no-prompt-templates",
                "--no-themes",
                "--no-context-files",
                "--no-approve",
            ])
            .env("PI_TELEMETRY", "0")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // 工具：给了白名单就按白名单开放，否则禁用全部工具（只做文本分析）。
        // 带工具时不能用「只输出 JSON summary」的分析用系统提示。
        match self.tools.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(tools) => {
                // 带工具时仍要满足 analysis_v1 的结果契约：最后一条消息是 JSON summary。
                command.args([
                    "--tools",
                    tools,
                    "--system-prompt",
                    "Carry out the task in the request using the available tools. When finished, your last message must be a JSON object with one nonempty string field named summary.",
                ]);
            }
            None => {
                command.args([
                    "--no-tools",
                    "--system-prompt",
                    "Analyze only the supplied request. Return exactly a JSON object with one nonempty string field named summary. Do not claim to have performed external actions.",
                ]);
            }
        }
        // 会话持久化：--no-session 是内存会话；恢复已有会话时用 --session。
        if self.session {
            if let Some(dir) = &self.session_dir {
                command.args(["--session-dir", &dir.to_string_lossy()]);
            }
            if let Some(session_id) = request
                .context
                .as_ref()
                .and_then(|c| c.resume_session_id.clone())
            {
                command.args(["--session", &session_id]);
            }
        } else {
            command.arg("--no-session");
        }
        if let Some(model) = &self.model {
            command.args(["--model", model]);
        }
        if let Some(provider) = &self.provider {
            command.args(["--provider", provider]);
        }
        // 密钥只走环境变量；配置与日志里只有变量名。
        if let Some(env_name) = &self.api_key_env {
            let value = std::env::var(env_name).map_err(|_| {
                Ru41Error::Config(format!(
                    "Pi api_key_env {env_name} is not set in the environment"
                ))
            })?;
            command.env(env_name, value);
        }
        // 自定义端点：临时 agent 目录 + models.json。
        if self.base_url.is_some() {
            let agent_dir = self.write_models_json(&request.run_dir)?;
            command.env("PI_CODING_AGENT_DIR", agent_dir);
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
        let prompt = crate::skills::render_for_prompt(
            &request.prompt,
            request
                .context
                .as_ref()
                .map_or(&[][..], |context| context.skills.as_slice()),
        );
        let protocol = async {
            stdin
                .write_all(
                    format!(
                        "{}\n",
                        json!({"id":"ru41-prompt","type":"prompt","message":prompt})
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
            let mut usage: Option<TokenUsage> = None;
            let mut session_id: Option<String> = None;
            let mut asked_state = false;
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
                        // 会话模式下问一次会话 ID，供后续恢复使用。
                        // Pi 若已退出，写入会 broken pipe；这只是可选查询，
                        // 不能因此判定整次运行失败。
                        if self.session && !asked_state {
                            asked_state = true;
                            let query =
                                format!("{}\n", json!({"id":"ru41-state","type":"get_state"}));
                            let _ = stdin.write_all(query.as_bytes()).await;
                            let _ = stdin.flush().await;
                        }
                    }
                    Some("response") if event["id"] == "ru41-state" => {
                        if event["success"] == true {
                            session_id = session_id_from_state(&event["data"]);
                        }
                    }
                    Some("message_update") => {
                        if let Some(reported) = usage_from_event(&event) {
                            usage = Some(reported);
                        }
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
                        session_id,
                        usage,
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
