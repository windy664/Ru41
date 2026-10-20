use crate::{
    app::obsidian::append_quick_note,
    ports::{
        commands::{CommandContext, CommandHandler, Reply},
        vault::Vault,
    },
};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// QQ 快速记录命令：把一句话追加进 Obsidian Inbox 并提交推送。
/// 私人内容：仅绑定用户在单聊中可用（与水电同一隐私边界）。
/// Vault 写入走核心服务（`ports::vault`），本模块只做鉴权与交互。
pub struct ObsidianQqCommand {
    vault: Arc<dyn Vault>,
    inbox_dir: String,
    offset_seconds: i32,
}

impl ObsidianQqCommand {
    pub fn new(vault: Arc<dyn Vault>, inbox_dir: String, offset_seconds: i32) -> Self {
        Self {
            vault,
            inbox_dir,
            offset_seconds,
        }
    }
}

impl CommandHandler for ObsidianQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "记一下" | "想法" | "note" | "idea")
    }

    fn help(&self) -> &'static str {
        "`/记一下 <内容>`：把想法写进 Obsidian 收件箱（单聊，需绑定）"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if arg.trim().is_empty() {
                return Ok(None);
            }
            if let Some(reply) =
                ctx.require_private("记录想法", "想法属于个人内容，群聊不写入笔记")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let path = append_quick_note(
                self.vault.as_ref(),
                &self.inbox_dir,
                arg,
                crate::util::now(),
                self.offset_seconds,
            )?;
            Ok(Some(Reply::text(format!(
                "## 已记录\n\n已写入 `{path}` 并推送，后台整理后会归档到合适位置。"
            ))))
        })
    }
}
