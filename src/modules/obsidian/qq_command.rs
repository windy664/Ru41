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
///
/// 两种入口：`/记一下 <内容>` 显式命令，或自然语言前缀
/// （「记一下：」「记住：」等，见 TRIGGER_PREFIXES）。
pub struct ObsidianQqCommand {
    vault: Arc<dyn Vault>,
    inbox_dir: String,
    offset_seconds: i32,
}

/// 自然语言捕获的触发前缀；长的在前，避免「记录一下」被「记一下」截断。
/// 只做整句前缀匹配——闲聊句中路过这三个字不算记录意图。
const TRIGGER_PREFIXES: &[&str] = &[
    "帮我记一下",
    "记录一下",
    "记一下",
    "记住",
    "note:",
    "remember:",
];

/// 剥掉触发前缀与紧随的分隔符，取出正文；不是捕获语则返回 None。
fn extract_note(content: &str) -> Option<String> {
    let trimmed = content.trim();
    let lower = trimmed.to_lowercase();
    let prefix = TRIGGER_PREFIXES
        .iter()
        .find(|prefix| lower.starts_with(**prefix))?;
    // 触发词不含大小写敏感字符，lower 与原文前缀等长，按字节切安全。
    let rest = trimmed[prefix.len()..]
        .trim_start_matches(['：', ':', '，', ',', '、', ' '])
        .trim();
    Some(rest.to_owned())
}

impl ObsidianQqCommand {
    pub fn new(vault: Arc<dyn Vault>, inbox_dir: String, offset_seconds: i32) -> Self {
        Self {
            vault,
            inbox_dir,
            offset_seconds,
        }
    }

    /// 鉴权 + 写入收件箱；handle 与 handle_message 共用。
    fn record<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(reply) =
                ctx.require_private("记录想法", "想法属于个人内容，群聊不写入笔记")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let path = append_quick_note(
                self.vault.as_ref(),
                &self.inbox_dir,
                content,
                crate::util::now(),
                self.offset_seconds,
            )?;
            Ok(Some(Reply::text(format!(
                "## 已记录\n\n已写入 `{path}` 并推送，后台整理后会归档到合适位置。"
            ))))
        })
    }
}

impl CommandHandler for ObsidianQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "记一下" | "想法" | "note" | "idea")
    }

    fn help(&self) -> &'static str {
        "`/记一下 <内容>`：写进 Obsidian 收件箱（单聊，需绑定）；直接说「记一下：内容」也行"
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
            self.record(arg, ctx).await
        })
    }

    /// 自然语言捕获：整句以触发前缀开头即视为记录意图，不写命令词。
    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            let Some(note) = extract_note(content) else {
                return Ok(None);
            };
            if note.is_empty() {
                return Ok(Some(Reply::text(
                    "## 记一下\n\n触发词后跟上内容即可，例如：\n`记一下：周六下午三点社团开会`",
                )));
            }
            self.record(&note, ctx).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::extract_note;

    #[test]
    fn extracts_content_after_trigger_and_separator() {
        assert_eq!(
            extract_note("记一下：周六下午三点社团开会").as_deref(),
            Some("周六下午三点社团开会")
        );
        assert_eq!(
            extract_note("记住 导师邮箱是 a@b.com").as_deref(),
            Some("导师邮箱是 a@b.com")
        );
        assert_eq!(
            extract_note("帮我记一下，周三交实验报告").as_deref(),
            Some("周三交实验报告")
        );
        assert_eq!(
            extract_note("Note: buy milk").as_deref(),
            Some("buy milk")
        );
    }

    #[test]
    fn bare_trigger_returns_empty_and_chat_is_untouched() {
        assert_eq!(extract_note("记一下").as_deref(), Some(""));
        // 句子中间出现触发词不捕获
        assert!(extract_note("你帮我记一下好不好").is_none());
        assert!(extract_note("今天天气不错").is_none());
        assert!(extract_note("/记一下 显式命令走 handle").is_none());
    }
}
