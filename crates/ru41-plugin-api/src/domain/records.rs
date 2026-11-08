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
    /// 通知路由：worker 注入，dispatcher 按它把卡片发回会话；None 不推送。
    #[serde(default)]
    pub notify: Option<super::NotifyRoute>,
    /// 到期未答复时的自动回复（dismiss 语义：用户关掉问题就当没问）。
    /// 审批类填"拒绝"，提问类填"用户未答复，按你的判断继续"；
    /// None 维持旧行为（过期即错误）。
    #[serde(default)]
    pub dismiss_response: Option<Value>,
    /// 卡片已转发到渠道（dispatcher 写，防重发）。
    #[serde(default)]
    pub notified: bool,
}
