use crate::domain::records::Interaction;
use anyhow::Result;
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// 底座交互的转发边界：把底座发出的审批/补充信息请求送达用户并等待决定。
/// 实现负责持久化请求（防丢）、送达任一入口（QQ/HTTP）并返回用户选择；
/// 返回值必须属于 `Interaction::options` 之一，由宿主裁决层校验。
pub trait InteractionHandler: Send + Sync {
    /// 挂起直到用户回复、请求过期或运行终止；过期时返回错误而非永久阻塞。
    fn request(
        &self,
        request: Interaction,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + '_>>;
}
