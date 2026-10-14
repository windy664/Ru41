//! 外部 Agent 的协议适配层；统一契约位于 ports::agent，启动装配位于 bootstrap::agents。
pub mod codex;
pub mod pi;
mod process;
