//! 凭据托管的数据约定：凭据只有**唯一标识**，不分类、不解释。
//!
//! 与调研到的 SecretRef/broker 模式一致（1Password Secure Agentic Autofill、
//! Auth0 Agent Core、OpenClaw RFC #9676）：调用方按唯一 ID 引用凭据，
//! 真值只在执行期注入，永远不进模型上下文。描述性文字只是标签
//! （给人看：名单里能认出这是哪条），不参与寻址、也不构成层级。

use rand::Rng;
use serde::{Deserialize, Serialize};

/// 凭据记录类别（`records` 表分区）。
pub const KIND: &str = "credential";

/// 凭据唯一标识：`cred_` + 12 位十六进制。
pub const ID_PREFIX: &str = "cred_";

/// 生成凭据唯一标识（随机、不可预测——ID 会出现在笔记里，别让它可枚举）。
pub fn new_id() -> String {
    let mut rng = rand::rng();
    let hex: String = (0..12)
        .map(|_| format!("{:x}", rng.random_range(0..16)))
        .collect();
    format!("{ID_PREFIX}{hex}")
}

/// 是否形如凭据标识。
pub fn is_id(text: &str) -> bool {
    text.len() == ID_PREFIX.len() + 12
        && text.starts_with(ID_PREFIX)
        && text[ID_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
}

/// 一条凭据（`records` 里 `credential` 类的 body）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// 真值：只在宿主侧流转，不进模型上下文。
    pub value: String,
    /// 人类可读的描述（标签）：名单里用来认出这条凭据是干什么的。
    #[serde(default)]
    pub label: String,
    /// 更新时间（epoch 秒）。
    #[serde(default)]
    pub updated_at: i64,
}
