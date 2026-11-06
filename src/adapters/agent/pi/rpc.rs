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

/// 候选模型：供应方 + 模型 ID。fallback 链里逐个尝试。
#[derive(Clone, Debug)]
struct Candidate {
    model: String,
    provider: Option<String>,
}

/// 单次运行的工具调用硬上限。pi 无原生迭代上限，放任下去模型会在
/// 「下载大文件/装依赖/逐页渲染」这类黑洞任务上烧满整个执行预算
/// （实测：为查一项奖学金条款去下载 61MB 手册 PDF 并逐页渲染）。
/// 超限按 InvalidOutput 归类为永久失败——同样的提示词重试只会重蹈覆辙。
const TOOL_EXECUTION_BUDGET: usize = 24;

impl Candidate {
    /// 展示名（供应方/模型），用于日志与用量归属。
    fn label(&self) -> String {
        match &self.provider {
            Some(provider) if !provider.is_empty() => format!("{provider}/{}", self.model),
            _ => self.model.clone(),
        }
    }
}

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
    /// 显式加载的 Pi 扩展（--extension）；扩展发现始终关闭。
    pub extensions: Vec<String>,
    /// 额外注入子进程的环境变量（扩展读配置用）。
    pub extra_env: std::collections::BTreeMap<String, String>,
    /// 备用模型链：主模型失败（限流/服务端错误/协议中断）时按顺序切换。
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

/// 从模型输出里取出 JSON 对象。
/// Pi 没有 codex 那样的输出 schema 强制，模型常把 JSON 包在 ```json 代码块里
/// 或前后加说明；这里取第一个配平的 `{...}` 再解析，别让格式噪声废掉整次运行。
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        let ch = *byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 解析 agent 结果：优先整体 JSON，其次取第一个配平的 JSON 对象；
/// 都不是 JSON 时把最后一条文本当 summary（闲聊与简单问答常不按契约输出，
/// 那条文本本身就是答案，整条任务判失败反而更糟——与 codex 的宽松口径一致）。
/// 取一条 assistant 消息里的全部文本块。
fn message_text(message: &Value) -> String {
    let mut text = String::new();
    for block in message["content"].as_array().into_iter().flatten() {
        if block["type"] == "text"
            && let Some(chunk) = block["text"].as_str()
        {
            text.push_str(chunk);
        }
    }
    text
}

/// 挑出「完整正文」候选：模型把报告写在前面一条消息里，最后一条 JSON 的
/// summary 反而是压缩版时，返回那份正文（2026-11-06 晚报实测）。
///
/// 判定要保守：正文得明显更长（2 倍以上）且本身就是一份像样的交付物，
/// 否则宁可交付 summary——压缩摘要也是模型自己的选择。
fn pick_full_report(candidates: &[String], summary: &str) -> Option<String> {
    let summary_len = summary.trim().chars().count();
    let best = candidates
        .iter()
        .filter(|c| {
            c.trim().chars().count() > summary_len.saturating_mul(2)
                && c.trim().chars().count() > 800
        })
        .max_by_key(|c| c.trim().chars().count())?;
    // 正文本身得像交付物：带 Markdown 标题或分节，否则可能只是过程絮叨。
    let looks_like_report = best.starts_with('#')
        || best.contains("\n## ")
        || best.contains("\n- **")
        || best.contains("\n### ");
    looks_like_report.then(|| best.trim().to_owned())
}

fn parse_agent_result(text: &str) -> Result<AgentResult> {
    let trimmed = text.trim();
    let candidate = extract_json_object(trimmed).unwrap_or(trimmed);
    match serde_json::from_str::<AgentResult>(candidate) {
        Ok(result) => {
            result.validate().map_err(Ru41Error::InvalidOutput)?;
            Ok(result)
        }
        Err(_) => {
            if trimmed.is_empty() {
                return Err(
                    Ru41Error::InvalidOutput("Pi result must be JSON with summary".into()).into(),
                );
            }
            Ok(AgentResult {
                summary: trimmed.to_owned(),
            })
        }
    }
}

impl PiAgent {
    /// 兼容端点配置：models.json 内容；密钥以 `$ENV` 引用写入，真实值只留在进程环境里。
    pub fn models_json(&self, model: &str, provider: Option<&str>) -> Result<Value> {
        let api = self.api.as_deref().unwrap_or("anthropic-messages");
        ensure!(
            SUPPORTED_APIS.contains(&api),
            Ru41Error::Config(format!(
                "pi api must be one of {} (got {api})",
                SUPPORTED_APIS.join(", ")
            ))
        );
        let provider_id = provider
            .filter(|p| !p.is_empty())
            .unwrap_or("ru41")
            .to_owned();
        let model_id = model.rsplit_once('/').map_or(model, |(_, id)| id);
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
    fn write_models_json(&self, run_dir: &Path, candidate: &Candidate) -> Result<PathBuf> {
        let root = self.models_json(&candidate.model, candidate.provider.as_deref())?;
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
    /// 候选链：主模型在前，备用模型按配置顺序在后。
    fn candidates(&self) -> Vec<Candidate> {
        let mut list = Vec::new();
        match &self.model {
            Some(model) => list.push(Candidate {
                model: model.clone(),
                provider: self.provider.clone(),
            }),
            // 没配模型时交给 Pi 用它自己的默认模型（候选的 model 为空表示不传 --model）。
            None if self.fallback_models.is_empty() => list.push(Candidate {
                model: String::new(),
                provider: self.provider.clone(),
            }),
            None => {}
        }
        for fallback in &self.fallback_models {
            let (provider, model) = match fallback.split_once('/') {
                Some((provider, model)) => (Some(provider.to_owned()), model.to_owned()),
                None => (self.provider.clone(), fallback.clone()),
            };
            list.push(Candidate { model, provider });
        }
        list
    }

    /// 换模型也救不了的错误：契约不支持、配置缺失、结果不合约。
    fn is_fatal(error: &anyhow::Error) -> bool {
        matches!(
            error.downcast_ref::<Ru41Error>(),
            Some(Ru41Error::UnsupportedContract(_))
                | Some(Ru41Error::Config(_))
                | Some(Ru41Error::InvalidOutput(_))
        )
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
        let candidates = self.candidates();
        ensure!(
            !candidates.is_empty(),
            Ru41Error::Config("Pi requires a model".into())
        );
        if candidates.len() == 1 {
            return self.run_candidate(&request, &candidates[0]).await;
        }
        let mut last_error = None;
        for candidate in &candidates {
            match self.run_candidate(&request, candidate).await {
                Ok(output) => return Ok(output),
                Err(error) => {
                    if Self::is_fatal(&error) {
                        return Err(error);
                    }
                    eprintln!(
                        "[pi] model {} failed ({error:#}); trying next candidate",
                        candidate.label()
                    );
                    last_error = Some(error);
                }
            }
        }
        Err(last_error.unwrap_or_else(|| {
            Ru41Error::AgentProcess("Pi candidate chain exhausted".into()).into()
        }))
    }

    async fn run_candidate(
        &self,
        request: &AgentRequest,
        candidate: &Candidate,
    ) -> Result<AgentOutput> {
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
                     schedule) before answering. Keep tool use to a few calls: \
                     never download large files, never install packages or dependencies, \
                     never render or OCR documents page by page. \
                     When finished, your last message must be a JSON object with one \
                     nonempty string field named summary. The summary field must \
                     contain the COMPLETE deliverable (the full report or full answer, \
                     in Markdown) — do not answer in a message first and then compress \
                     it into a one-paragraph summary. If the tools cannot find it, \
                     say so in the summary instead of searching extensively.",
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
        if !candidate.model.is_empty() {
            command.args(["--model", &candidate.model]);
        }
        if let Some(provider) = &candidate.provider {
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
            let mut tool_calls = 0_usize;
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
                if let Some(file) = event_log.as_mut() {
                    use tokio::io::AsyncWriteExt;
                    let _ = file.write_all(&line).await;
                    let _ = file.write_all(b"\n").await;
                }
                let event: Value = serde_json::from_slice(&line)
                    .map_err(|_| Ru41Error::AgentProcess("invalid Pi JSONL record".into()))?;
                match event["type"].as_str() {
                    Some("tool_execution_start") => {
                        tool_calls += 1;
                        ensure!(
                            tool_calls <= TOOL_EXECUTION_BUDGET,
                            Ru41Error::InvalidOutput(format!(
                                "Pi exhausted its tool budget ({tool_calls} tool calls) without finishing"
                            ))
                        );
                    }
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
                        model: Some(candidate.label()),
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

#[cfg(test)]
mod tests {
    use super::{parse_agent_result, pick_full_report};

    /// 模型先写完整报告、最后一条 JSON 只给压缩摘要时，交付完整报告。
    #[test]
    fn picks_full_report_over_compressed_summary() {
        let report = format!(
            "# 今日机会雷达（2026-11-06）\n\n## 1. 某某模型发布\n\n- **链接**：https://x\n{}\n",
            "正文细节。".repeat(400)
        );
        let summary = "今日筛选出 7 条值得关注的条目：1) ……；2) ……";
        let picked = pick_full_report(std::slice::from_ref(&report), summary);
        assert_eq!(picked.as_deref(), Some(report.trim()));
    }

    /// 正文不够长、或不像交付物时，不动 summary。
    #[test]
    fn keeps_summary_when_no_full_report() {
        let summary = "今日筛选出 7 条值得关注的条目：1) ……";
        assert!(pick_full_report(&["短消息".to_owned()], summary).is_none());
        let chatter = "我先查一下接口。".repeat(200);
        assert!(pick_full_report(&[chatter], summary).is_none());
    }

    /// 最后一条不是 JSON 时，整段文本当 summary（既有兜底口径）。
    #[test]
    fn plain_text_becomes_summary() {
        let result = parse_agent_result("  直接一段话  ").unwrap();
        assert_eq!(result.summary, "直接一段话");
    }
}
