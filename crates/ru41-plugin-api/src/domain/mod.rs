pub mod consumers;
pub mod events;
pub mod records;
pub mod todo;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::ports::media::MediaRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    RetryScheduled,
    Succeeded,
    Failed,
    Cancelled,
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "retry_scheduled" => Ok(Self::RetryScheduled),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown task status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub request_key: String,
    pub backend_id: String,
    pub contract: RunContract,
    pub prompt: String,
    pub status: TaskStatus,
    pub created_at: String,
    pub updated_at: String,
    pub attempt_count: i64,
    pub max_attempts: i64,
    pub active_attempt_id: Option<i64>,
    pub lease_until: Option<i64>,
    pub available_at: i64,
    pub result: Option<AgentResult>,
    pub last_error: Option<String>,
    pub notify: Option<NotifyRoute>,
    pub notified_at: Option<String>,
}

/// 任务终态后的渠道通知路由；target 格式由各渠道适配器定义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyRoute {
    pub channel: String,
    pub target: String,
    /// 被动回复锚点（如 QQ 的 msg_id），主动推送渠道可为空。
    pub msg_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EnqueueResult {
    pub created: bool,
    pub task: Task,
}

/// 一次 agent 运行的结果：摘要正文 + 要随结果一起发给用户的媒体附件。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResult {
    pub summary: String,
    /// 随结果一起发给用户的富媒体（截图、图表、生成的文档…）。
    ///
    /// 底座在最终 JSON 里给出（路径相对运行目录），宿主解析后校验归属与大小，
    /// 再交给渠道上传。
    #[serde(default)]
    pub media: Vec<MediaRef>,
}

/// 单次结果最多带的媒体数。
pub const MAX_RESULT_MEDIA: usize = 8;

impl AgentResult {
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() || self.summary.len() > 65_536 {
            return Err("summary must contain 1..65536 bytes of nonblank text".into());
        }
        if self.media.len() > MAX_RESULT_MEDIA {
            return Err(format!(
                "media must contain at most {MAX_RESULT_MEDIA} items (got {})",
                self.media.len()
            ));
        }
        if self.media.iter().any(|m| m.source.trim().is_empty()) {
            return Err("media source must not be blank".into());
        }
        Ok(())
    }
}

/// 一次 agent 运行的 token 用量；来自底座事件流（codex `turn.completed` 的
/// usage 字段），底座不报就是 None。cached_input_tokens 计入 input_tokens。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// 多轮运行（resume 会话逐轮上报）的累加。
    pub fn accumulate(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
    }

    /// 从 codex 事件 JSON 提取 usage；字段缺失或全 0 视为底座未上报。
    pub fn from_event(event: &serde_json::Value) -> Option<TokenUsage> {
        let usage = event.get("usage")?;
        let field = |name: &str| {
            usage
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };
        let parsed = TokenUsage {
            input_tokens: field("input_tokens"),
            cached_input_tokens: field("cached_input_tokens"),
            output_tokens: field("output_tokens"),
            total_tokens: field("total_tokens"),
        };
        (parsed.total_tokens > 0 || parsed.input_tokens > 0 || parsed.output_tokens > 0)
            .then_some(parsed)
    }
}

#[derive(Debug, Serialize)]
pub struct Attempt {
    pub id: i64,
    pub task_id: i64,
    pub worker_id: String,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub run_dir: Option<String>,
    pub session_id: Option<String>,
    pub backend_id: String,
    pub model: Option<String>,
    pub contract: RunContract,
    pub result: Option<AgentResult>,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Claim {
    pub task: Task,
    pub attempt_id: i64,
}

/// 版本化的宿主级结果与执行契约。
/// 新的动作类型需要各自的校验/审批语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunContract {
    AnalysisV1,
}
impl RunContract {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnalysisV1 => "analysis_v1",
        }
    }
}
impl FromStr for RunContract {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "analysis_v1" => Ok(Self::AnalysisV1),
            _ => Err(format!("unsupported run contract: {value}")),
        }
    }
}
