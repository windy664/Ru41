use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub revision: i64,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub owner: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRun {
    pub id: String,
    pub task_id: i64,
    pub attempt_id: i64,
    pub backend_id: String,
    pub status: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    pub run_id: String,
    pub request_id: Value,
    pub method: String,
    pub detail: Value,
    pub options: Vec<Value>,
    pub expires_at: i64,
    pub status: String,
    pub response: Option<Value>,
}
