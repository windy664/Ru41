use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
/// 通用记录：kind + id 定位，revision 供乐观并发控制。
pub struct Document {
    pub id: String,
    pub revision: i64,
    pub value: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
/// 用户持续交互的会话上下文（预留；与渠道会话关联）。
pub struct Session {
    pub id: String,
    pub owner: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
/// 一次底座运行：任务/尝试引用、底座标识、状态与底座会话。
/// 运行成功不等于任务成功；任务成败由宿主判定。
pub struct AgentRun {
    pub id: String,
    pub task_id: i64,
    pub attempt_id: i64,
    pub backend_id: String,
    pub status: String,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
/// 底座发出的审批/补充信息请求及其裁决状态。
/// options 由适配器按底座原生协议提供；response 必须是其中之一。
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
