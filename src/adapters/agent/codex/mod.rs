//! Codex 接入：批处理与 app-server 会话共享模型提供方配置。
use anyhow::Result;

mod exec;
pub mod provider;
mod session;

pub use exec::{CodexAgent, version, version_with};
pub use session::CodexSession;

/// 底座沙箱模式。默认 Bypass 维持历史行为（全权限）；
/// WorkspaceWrite 收敛为「工作区内可写、命令需显式开网络」，
/// 把 prompt 注入或底座误操作的爆炸半径限制在 run_dir。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SandboxMode {
    #[default]
    Bypass,
    WorkspaceWrite,
}

impl SandboxMode {
    /// 解析配置值；None/空串回退默认 Bypass，未知值报错而非静默放行。
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value.unwrap_or("") {
            "" | "bypass" => Ok(Self::Bypass),
            "workspace-write" => Ok(Self::WorkspaceWrite),
            other => Err(anyhow::anyhow!(
                "agents.codex.sandbox 仅支持 bypass | workspace-write，收到 {other:?}"
            )),
        }
    }
}

fn ensure_tool_calls_executed(summary: &str) -> anyhow::Result<()> {
    if summary.contains("<tool_call_") && summary.contains("<function=") {
        return Err(crate::domain::error::Ru41Error::AgentProcess(
            "Codex returned an unexecuted tool call".into(),
        )
        .into());
    }
    Ok(())
}
