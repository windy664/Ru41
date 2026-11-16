//! QQ 内置命令：语义在 [`crate::channels::builtin`]，这里只固定 QQ 词表。
//!
//! 保留原有公开路径：插件测试与调用方都从这里取 `Command` / `parse_command` /
//! `execute` / `help_text`，行为与历史一致。

use crate::{
    channels::builtin::{self, Vocabulary},
    ports::commands::{CommandContext, CommandHandler, Reply},
};
use anyhow::Result;
use std::sync::Arc;

/// QQ 命令词表（中文裸词也能命中，与历史行为一致）。
pub const VOCABULARY: Vocabulary = builtin::DEFAULT;

pub use builtin::{Command, format_task_detail, format_task_list};

/// 解析 QQ 内置命令；命令词的参数非法时返回 None，让自然语言落入后续路径。
pub fn parse_command(content: &str) -> Option<Command> {
    builtin::parse_command(content, &VOCABULARY)
}

/// 执行 QQ 内置命令并返回回复文本；不直接发消息，便于测试。
pub fn execute(
    command: &Command,
    ctx: &CommandContext,
    pricing: &crate::app::usage_report::Pricing,
) -> Result<Reply> {
    builtin::execute(command, ctx, pricing, &VOCABULARY)
}

/// 内置命令与当前启用的扩展命令共同组成帮助文本。
pub fn help_text(handlers: &[Arc<dyn CommandHandler>]) -> String {
    builtin::help_text(handlers, &VOCABULARY)
}
