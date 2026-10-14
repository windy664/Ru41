//! 任务失败的可编程分类：瞬态按退避重试，永久直接终态、不消耗重试预算。
//!
//! 适配器把可判定的失败映射为 `Ru41Error`；Worker 经 `classify` 消费。
//! 未分类的未知错误默认瞬态（有界重试兜底），宁可多试不可漏试。

use anyhow::Error;

/// 失败类别：决定 Worker 是否安排重试。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// 瞬态：超时、进程崩溃、网络故障——退避后重试。
    Transient,
    /// 永久：配置错误、契约不支持、输出非法、审批过期——重试无意义。
    Permanent,
}

/// 宿主可判定的错误分类；适配器按语义映射，不透传底座原文。
#[derive(Debug, thiserror::Error)]
pub enum Ru41Error {
    /// 执行超时；通常是排队或网络抖动。
    #[error("agent execution timed out")]
    Timeout,
    /// 底座进程异常退出或协议中断。
    #[error("agent process failed: {0}")]
    AgentProcess(String),
    /// 底座产出的结果不满足运行契约。
    #[error("invalid agent output: {0}")]
    InvalidOutput(String),
    /// 任务声明的契约不被该底座支持。
    #[error("unsupported run contract: {0}")]
    UnsupportedContract(String),
    /// 配置或环境错误（如缺少可执行文件、坏连接串）。
    #[error("invalid configuration: {0}")]
    Config(String),
    /// 审批/交互请求在用户回复前过期。
    #[error("approval interaction expired")]
    ApprovalExpired,
}

impl Ru41Error {
    /// 本错误的失败类别。
    pub fn kind(&self) -> FailureKind {
        match self {
            Self::Timeout | Self::AgentProcess(_) => FailureKind::Transient,
            Self::InvalidOutput(_)
            | Self::UnsupportedContract(_)
            | Self::Config(_)
            | Self::ApprovalExpired => FailureKind::Permanent,
        }
    }
}

/// 对任意 anyhow 错误分类：内含 `Ru41Error` 用其类别，未知默认瞬态。
pub fn classify(error: &Error) -> FailureKind {
    error
        .downcast_ref::<Ru41Error>()
        .map_or(FailureKind::Transient, Ru41Error::kind)
}
