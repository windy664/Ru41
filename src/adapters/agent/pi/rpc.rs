use super::{
    super::process::{OwnedChild, ProcessGroup},
    contract::{message_text, parse_agent_result, pick_full_report},
    models::Candidate,
    steer::pump_steers,
    ui::pi_ui_interact,
};
// Pi 1.2 RPC 分析适配器。模型与认证由 Pi 管理，宿主只管理进程和结果契约。
use crate::{
    domain::{RunContract, error::Ru41Error},
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
    /// 显式加载的 Pi 扩展（--extension）；扩展发现始终关闭。
    pub extensions: Vec<String>,
    /// 额外注入子进程的环境变量（扩展读配置用）。
    pub extra_env: std::collections::BTreeMap<String, String>,
    /// 模型输入模态（text/image）：写进 models.json，声明 image 后 Pi 的
    /// read 工具才会把图片作为附件发给模型。
    pub input: Vec<String>,
    /// 备用模型链：主模型失败（限流/服务端错误/协议中断）时由 Pi virtual model 按顺序切换。
    /// 每项写 `供应方/模型` 或裸 `模型`（沿用主供应方）。
    pub fallback_models: Vec<String>,
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
            extensions: Vec::new(),
            extra_env: Default::default(),
            fallback_models: Vec::new(),
            input: vec!["text".to_owned(), "image".to_owned()],
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
    async fn execute(&self, request: AgentRequest) -> Result<AgentOutput> {
        ensure!(
            request.contract == RunContract::AnalysisV1,
            Ru41Error::UnsupportedContract(request.contract.as_str().into())
        );
        ensure!(
            self.provider.is_none() || self.model.is_some(),
            Ru41Error::Config("Pi provider requires model".into())
        );
        let candidates = self.candidates();
        ensure!(
            !candidates.is_empty(),
            Ru41Error::Config("Pi requires a model".into())
        );
        // fallback 由 Pi 原生 virtual model 在同一个进程里处理；Ru41 不再重启候选链。
        self.run_candidate(&request, &candidates).await
    }

    async fn run_candidate(
        &self,
        request: &AgentRequest,
        candidates: &[Candidate],
    ) -> Result<AgentOutput> {
        let candidate = &candidates[0];
        let fallback_extension = if candidates.len() > 1 {
            Some(self.write_fallback_extension(&request.run_dir, candidates)?)
        } else {
            None
        };
        let skills = request
            .context
            .as_ref()
            .map_or(&[][..], |context| context.skills.as_slice());
        // 会话目录按会话隔离：不同聊天各自一个子目录。
        let session_dir = if self.session {
            session_dir_for(
                self.session_dir.as_deref(),
                request
                    .context
                    .as_ref()
                    .and_then(|context| context.conversation_id.as_deref()),
            )
        } else {
            None
        };
        // 与 codex 同一套技能落盘：有工具时只给索引，模型按需 Read SKILL.md；
        // 无工具的纯文本任务读不了文件，仍内联全文。
        crate::skills::materialize(&request.run_dir, skills)?;
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
        // 扩展：只加载配置里显式写下的那几个。
        // --no-extensions 保留（不做扩展发现），但 Pi 允许显式 -e 覆盖它。
        for extension in &self.extensions {
            command.arg("--extension").arg(extension);
        }
        if let Some(extension) = &fallback_extension {
            command.arg("--extension").arg(extension);
        }
        for (key, value) in &self.extra_env {
            command.env(key, value);
        }
        // 工具：给了白名单就按白名单开放，否则禁用全部工具（只做文本分析）。
        // 带工具时不能用「只输出 JSON summary」的分析用系统提示。
        match self.tools.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(tools) => {
                // 带工具时仍要满足 analysis_v1 的结果契约：最后一条消息是 JSON summary。
                command.args([
                    "--tools",
                    tools,
                    "--system-prompt",
                    "Carry out the task in the request using the available tools. \
                     Look up anything factual with the tools: schedules, balances, \
                     deadlines, file contents and notes must be read, never recalled \
                     from memory or guessed. The request context is a hint, not the \
                     source of truth — if it does not contain the answer, use a tool \
                     (read/bash, or a CLI such as 'ru41 event list --days 1' for today's \
                     schedule) before answering. \
                     Use the minimum number of tool calls that directly answers the \
                     user's exact request. Do not inspect directories, run general \
                     --help commands, or gather adjacent/background data unless the \
                     user asked for it or the selected skill explicitly requires it. \
                     Stop once you have enough evidence for the requested scope. \
                     When finished, your last message must be a JSON object with one \
                     nonempty string field named summary. The summary field must \
                     contain the COMPLETE deliverable (the full report or full answer, \
                     in Markdown) — do not answer in a message first and then compress \
                     it into a one-paragraph summary. If the tools cannot find it, \
                     say so in the summary. \
                     If you produced files the user should SEE (a screenshot, a chart, \
                     a generated document), also include a media array in that JSON: \
                     items are either a path relative to your working directory, or \
                     objects {kind, source, filename} where kind is image|video|voice|file. \
                     Only list files you actually created in this run; paths outside the \
                     working directory are dropped.",
                ]);
            }
            None => {
                command.args([
                    "--no-tools",
                    "--system-prompt",
                    "Analyze only the supplied request. Return exactly a JSON object with one nonempty string field named summary. The summary field must contain the complete answer (Markdown), not a compressed abstract of it. Do not claim to have performed external actions.",
                ]);
            }
        }
        // 会话持久化：--no-session 是内存会话；续接上一轮时用 --fork。
        //
        // 为什么是 fork 而不是 --session：pi 的会话按**项目目录**归属，而每次运行
        // 的工作目录是独立的 run_dir（/data/runs/task-N-attempt-M-xxxx）。用
        // `--session <id>` 恢复时 pi 会发现会话属于另一个项目，转成交互式询问
        // 「Fork this session into current directory?」；RPC 模式无人应答，pi
        // 直接退出，整次运行失败（2026-11-12 实测：第二条消息续接失败、
        // 重试后另起新会话，上下文就丢了）。--fork 非交互，且正好把上一轮历史
        // 带进本轮工作目录：既保住上下文，又保住每次运行的目录隔离。
        if self.session {
            if let Some(dir) = &session_dir {
                // 会话目录要跨容器滚动存活（挂 /data 之类），这里先建好，
                // 免得 pi 因为目录不存在起不来。
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("cannot create Pi session dir {}", dir.display()))?;
                command.args(["--session-dir", &dir.to_string_lossy()]);
            }
            if let Some(session_id) = request
                .context
                .as_ref()
                .and_then(|c| c.resume_session_id.clone())
            {
                command.args(["--fork", &session_id]);
            }
        } else {
            command.arg("--no-session");
        }
        if fallback_extension.is_some() {
            command.args(["--model", "ru41/auto"]);
        } else {
            if !candidate.model.is_empty() {
                command.args(["--model", &candidate.model]);
            }
            if let Some(provider) = &candidate.provider {
                command.args(["--provider", provider]);
            }
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
            let agent_dir = self.write_models_json(&request.run_dir, candidate)?;
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
        let prompt = if self
            .tools
            .as_deref()
            .is_some_and(|tools| !tools.trim().is_empty())
        {
            crate::skills::skills_index_prefix(skills) + &request.prompt
        } else {
            crate::skills::render_for_prompt(&request.prompt, skills)
        };
        let mut cancel_rx = request.cancel.clone();
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
            // 底座事件原样落盘：codex 侧有 events.jsonl，pi 侧此前只解析不留档，
            // 导致「这次用了哪些工具/技能」在 QQ 对话里查不到（worker 侧才看得到）。
            let mut event_log = tokio::fs::File::create(request.run_dir.join("events.jsonl"))
                .await
                .ok();
            let mut final_message: Option<Value> = None;
            // 每条 assistant 消息的正文（用于「模型先给报告、再给压缩摘要」的兜底）
            let mut report_candidates: Vec<String> = Vec::new();
            let mut accepted = false;
            let mut settled = false;
            let mut total = 0_usize;
            let mut usage: Option<TokenUsage> = None;
            let mut session_id: Option<String> = None;
            let mut actual_model: Option<String> = None;
            let mut asked_state = false;
            let mut steer_tick = tokio::time::interval(std::time::Duration::from_secs(2));
            steer_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                let mut line = Vec::new();
                let n = {
                    let mut limited = (&mut reader).take(1_048_577);
                    tokio::select! {
                        biased;
                        n = limited.read_until(b'\n', &mut line) => n?,
                        _ = steer_tick.tick() => {
                            if let Some(run_context) = request.context.as_ref() {
                                pump_steers(run_context, &mut stdin).await?;
                            }
                            continue;
                        }
                        _ = async {
                            match cancel_rx.as_mut() {
                                Some(rx) => rx.changed().await.map(|_| ()).unwrap_or(()),
                                None => std::future::pending::<()>().await,
                            }
                        } => {
                            // 协作式取消：走 pi 原生 abort 让它收尾（落会话状态），
                            // 宽限后仍不退出则由 kill_on_drop 兜底。
                            let abort = format!("{}\n", json!({"id":"ru41-abort","type":"abort"}));
                            let _ = stdin.write_all(abort.as_bytes()).await;
                            let _ = stdin.flush().await;
                            continue;
                        }
                    }
                };
                ensure!(
                    n > 0,
                    Ru41Error::AgentProcess("Pi exited before completion".into())
                );
                total += n;
                ensure!(
                    n <= 1_048_576 && total <= 16 * 1_048_576,
                    Ru41Error::AgentProcess("Pi protocol output exceeded limit".into())
                );
                if let Some(file) = event_log.as_mut() {
                    use tokio::io::AsyncWriteExt;
                    let _ = file.write_all(&line).await;
                    let _ = file.write_all(b"\n").await;
                }
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
                        if let (Some(provider), Some(model)) = (
                            event["message"]["provider"].as_str(),
                            event["message"]["model"].as_str(),
                        ) {
                            actual_model = Some(format!("{provider}/{model}"));
                        }
                        // 权威用量在最终消息上：Anthropic 协议端点（如 MiniMax）
                        // 流式期间 message_update 一直是 0，只有收尾才给真值。
                        if let Some(reported) = usage_from_event(&event["message"]) {
                            usage = Some(reported);
                        }
                        // 记下正文候选：有些模型会先把完整报告写出来，最后一条消息
                        // 只给一句压缩摘要（2026-11-06 晚报就是这样，日报正文被丢掉）。
                        let body = message_text(&event["message"]);
                        if !body.trim().is_empty() {
                            report_candidates.push(body);
                        }
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
                        // pi 原生交互（select/confirm）经宿主交互通道转发给用户
                        // （QQ 卡片 + /答复）；input/editor 等暂不接的按 cancelled
                        // 回复，pi 走自己的默认路径，运行不中断。
                        let ui_id = event["id"].clone();
                        let ui_method = event["method"].as_str().unwrap_or("");
                        let response = match ui_method {
                            "select" | "confirm" | "input" | "editor" => {
                                match request.context.as_ref() {
                                    Some(context) => pi_ui_interact(context, &event).await?,
                                    // 无交互通道（不应发生）：按取消处理。
                                    None => {
                                        json!({"type":"extension_ui_response","id":ui_id,"cancelled":true})
                                    }
                                }
                            }
                            _ => {
                                json!({"type":"extension_ui_response","id":ui_id,"cancelled":true})
                            }
                        };
                        stdin
                            .write_all(format!("{}\n", response).as_bytes())
                            .await?;
                        stdin.flush().await?;
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
                    let mut result = parse_agent_result(&text)?;
                    // 兜底：最后一条消息的 summary 是压缩版，而前面某条消息里有
                    // 完整正文（Markdown 报告）——交付完整正文，别交付压缩版。
                    if let Some(report) = pick_full_report(&report_candidates, &result.summary) {
                        result.summary = report;
                    }
                    stdin.shutdown().await?;
                    return Ok::<_, anyhow::Error>(AgentOutput {
                        result,
                        session_id,
                        usage,
                        model: actual_model.or_else(|| Some(candidate.label())),
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
        // 每一轮续接都会 fork 出一个新会话文件（历史逐轮复制），
        // 只留当前会话与最近一个旧文件，别让磁盘和 inode 无限涨。
        if let Ok(output) = &outcome
            && let (Some(dir), Some(session)) = (&session_dir, output.session_id.as_deref())
        {
            prune_session_forks(dir, session);
        }
        outcome
    }
}

/// 每个会话一个子目录：不同聊天互不干扰，清理旧 fork 时也不会误删别人的会话。
fn session_dir_for(base: Option<&Path>, conversation_id: Option<&str>) -> Option<PathBuf> {
    let base = base?;
    let Some(conversation) = conversation_id else {
        return Some(base.to_path_buf());
    };
    use sha1::Digest;
    let digest = sha1::Sha1::digest(conversation.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Some(base.join(&hex[..16]))
}

/// 清理本会话的旧 fork 文件：保留当前会话文件与最近一个旧文件（兜底），其余删除。
fn prune_session_forks(dir: &Path, keep_session_id: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    let mut spare = false;
    for path in files.iter().rev() {
        let is_current = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(keep_session_id));
        if is_current {
            continue;
        }
        if !spare {
            spare = true;
            continue;
        }
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::{prune_session_forks, session_dir_for};
    use std::path::Path;

    /// 不同会话要落在不同子目录：否则清理旧 fork 会误删别人的会话。
    #[test]
    fn session_dirs_are_isolated_per_conversation() {
        let base = Path::new("/data/pi-sessions");
        let first = session_dir_for(Some(base), Some("qq:user:U1")).unwrap();
        let second = session_dir_for(Some(base), Some("qq:user:U2")).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with(base));
        // 同一会话永远落在同一目录。
        assert_eq!(
            first,
            session_dir_for(Some(base), Some("qq:user:U1")).unwrap()
        );
        // 没配目录就没有会话目录。
        assert!(session_dir_for(None, Some("qq:user:U1")).is_none());
    }

    /// 清理旧 fork：保留当前会话文件与最近一个旧文件，其余删除。
    #[test]
    fn prune_keeps_current_and_one_spare() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "2026-11-12T01-00-00Z_aaa.jsonl",
            "2026-11-12T02-00-00Z_bbb.jsonl",
            "2026-11-12T03-00-00Z_ccc.jsonl",
            "2026-11-12T04-00-00Z_cur.jsonl",
        ] {
            std::fs::write(dir.path().join(name), b"{}").unwrap();
        }
        prune_session_forks(dir.path(), "cur");
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), 2, "{left:?}");
        assert!(left.iter().any(|name| name.contains("cur")));
    }
}
