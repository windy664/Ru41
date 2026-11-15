//! Telegram 内置命令：语义在 [`crate::channels::builtin`]，这里只固定 Telegram 词表。
//!
//! Telegram 的命令名只允许拉丁字母数字下划线（32 字符内），因此内置命令用
//! ASCII 词（`/help` `/tasks` `/task` `/cancel` `/bind` `/new` `/usage`）。
//! 插件的中文命令（如 `/精读`、`/课表`）仍可直接发文本命中，只是不出现在
//! Telegram 的命令菜单里。

use crate::{
    channels::builtin::{self, Vocabulary},
    ports::commands::{CommandContext, CommandHandler, Reply},
};
use anyhow::Result;
use std::sync::Arc;

/// Telegram 命令词表。
pub const VOCABULARY: Vocabulary = builtin::TELEGRAM;

pub use builtin::{Command, format_task_detail, format_task_list};

/// 解析 Telegram 内置命令。
pub fn parse_command(content: &str) -> Option<Command> {
    builtin::parse_command(content, &VOCABULARY)
}

/// 执行 Telegram 内置命令并返回回复文本；不直接发消息，便于测试。
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
