use serde::{Deserialize, Serialize};

/// 持久化业务事件。只携带状态和引用；敏感正文通过已有鉴权查询接口读取。
/// id 是当前数据库内按提交顺序排列的游标，不是跨数据库的全局标识。
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DomainEvent {
    pub id: i64,
    pub version: i32,
    pub kind: String,
    pub occurred_at: String,
    pub resource_id: String,
    pub task_id: Option<i64>,
    pub attempt_id: Option<i64>,
    pub run_id: Option<String>,
    pub status: String,
}
