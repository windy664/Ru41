use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Cancelled,
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(format!("unknown task status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub request_key: String,
    pub prompt: String,
    pub status: TaskStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct EnqueueResult {
    pub created: bool,
    pub task: Task,
}
