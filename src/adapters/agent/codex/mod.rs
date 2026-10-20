//! Codex 接入：批处理与 app-server 会话共享模型提供方配置。
mod exec;
pub mod provider;
mod session;

pub use exec::{CodexAgent, version, version_with};
pub use session::CodexSession;

fn ensure_tool_calls_executed(summary: &str) -> anyhow::Result<()> {
    if summary.contains("<tool_call_") && summary.contains("<function=") {
        return Err(crate::domain::error::Ru41Error::AgentProcess(
            "Codex returned an unexecuted tool call".into(),
        )
        .into());
    }
    Ok(())
}
