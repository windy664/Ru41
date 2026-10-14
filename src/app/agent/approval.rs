//! 审批转发契约（本次只建契约，不接实现）。
//!
//! 设计要点：
//! - 每个底座可能在执行中发起自己的审批请求（如 Codex 的命令执行 / 补丁审批），
//!   Ru41 的角色是转发：底座的审批事件 → 用户的决定 → 回传底座。
//! - 所有底座的审批统一走这里的契约，渠道侧（QQ / 飞书 / 钉钉）只面对 ApprovalRequest。
//! - 审批内容出站给用户前先过 Sanitizer 脱敏，避免把敏感信息带进消息渠道。
//! - 接入 AgentRequest / Worker 与 Codex app-server 对接是后续任务。

use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// 审批类别；新动作类型按底座真实触发补充。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    CommandExecution,
    ApplyPatch,
}

/// 一次待决审批：标题给人快速判断，detail 带完整上下文。
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub kind: ApprovalKind,
    pub title: String,
    pub detail: String,
}

/// 用户决定：批准 / 拒绝 / 中止整个运行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    Deny,
    Abort,
}

/// 审批转发边界；实现负责把请求送达用户并等待决定。
pub trait ApprovalHandler: Send + Sync {
    fn approve(
        &self,
        request: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalDecision>> + Send + '_>>;
}

impl<T: ApprovalHandler + ?Sized> ApprovalHandler for Arc<T> {
    fn approve(
        &self,
        request: ApprovalRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ApprovalDecision>> + Send + '_>> {
        (**self).approve(request)
    }
}
