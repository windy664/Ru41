pub mod consumers;
pub mod error;
pub mod events;
pub mod records;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentResult {
    pub summary: String,
}

impl AgentResult {
    pub fn validate(&self) -> Result<(), String> {
        if self.summary.trim().is_empty() || self.summary.len() > 65_536 {
            return Err("summary must contain 1..65536 bytes of nonblank text".into());
        }
        Ok(())
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
