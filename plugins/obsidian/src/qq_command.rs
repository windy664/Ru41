use anyhow::Result;
use ru41::{
    app::obsidian::append_quick_note,
    ports::{
        commands::{CommandContext, Reply, SimpleCommand},
        vault::Vault,
    },
};
use std::sync::Arc;

/// QQ 快速记录命令：把一句话追加进 Obsidian Inbox 并提交推送。
/// 私人内容：仅绑定用户在单聊中可用（与水电同一隐私边界）。
/// Vault 写入走核心服务（`ports::vault`），本模块只做鉴权与交互。
///
/// 两种入口：`/记一下 <内容>` 显式命令，或自然语言前缀
/// （`#临时想法`、`记一下：`、`记住：` 等，见 TRIGGER_PREFIXES）。
pub struct ObsidianQqCommand {
    vault: Arc<dyn Vault>,
    inbox_dir: String,
    offset_seconds: i32,
}

/// 自然语言捕获的触发前缀；长的在前，避免「记录一下」被「记一下」截断。
/// 只做整句前缀匹配——闲聊句中路过这几个字不算记录意图。
const TRIGGER_PREFIXES: &[&str] = &[
    "帮我记一下",
    "记录一下",
    "记一下",
    "记住",
    "别忘了",
    "别忘记",
    "提醒我",
    "记着",
    "note:",
    "remember:",
];

/// 剥掉 `#` 或触发前缀与紧随的分隔符，取出正文；不是捕获语则返回 None。
fn extract_note(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if let Some(rest) = trimmed.strip_prefix('#') {
        return Some(rest.trim().to_owned());
    }
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

    /// 鉴权 + 写入收件箱；run 与 run_message 共用。
    async fn record(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        if let Some(reply) =
            ctx.require_private("记录想法", "想法属于个人内容，群聊不写入笔记")?
        {
            return Ok(Some(Reply::text(reply)));
        }
        let path = append_quick_note(
            self.vault.as_ref(),
            &self.inbox_dir,
            content,
            ru41::util::now(),
            self.offset_seconds,
        )?;
        Ok(Some(Reply::text(format!(
            "## 已记录\n\n已写入 `{path}` 并推送，每周整理时会归档到合适位置。"
        ))))
    }
}

impl SimpleCommand for ObsidianQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &["记一下", "想法", "note", "idea"]
    }

    fn help(&self) -> &'static str {
        "`#临时想法` 或 `/记一下 <内容>`：写进 Obsidian 收件箱（单聊，需绑定）；整理时自动识别待办"
    }

    fn help_section(&self) -> &'static str {
        "记录与日程"
    }

    async fn run(&self, arg: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        if arg.trim().is_empty() {
            return Ok(None);
        }
        self.record(arg, ctx).await
    }

    /// 自然语言捕获：整句以 `#` 或触发前缀开头即视为记录意图。
    async fn run_message(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        let Some(note) = extract_note(content) else {
            return Ok(None);
        };
        if note.is_empty() {
            return Ok(Some(Reply::text(
                "## 记一下\n\n在 `#` 后跟上内容即可，例如：\n`#周六下午三点社团开会`",
            )));
        }
        self.record(&note, ctx).await
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
            extract_note("别忘了明天交作业").as_deref(),
            Some("明天交作业")
        );
        assert_eq!(
            extract_note("提醒我，下午三点开会").as_deref(),
            Some("下午三点开会")
        );
        assert_eq!(extract_note("记着买火车票").as_deref(), Some("买火车票"));
        assert_eq!(extract_note("Note: buy milk").as_deref(), Some("buy milk"));
    }

    #[test]
    fn hash_prefix_captures_quick_notes() {
        assert_eq!(extract_note("#一个临时想法").as_deref(), Some("一个临时想法"));
        assert_eq!(extract_note("# 明天交实验报告").as_deref(), Some("明天交实验报告"));
        assert_eq!(extract_note("#").as_deref(), Some(""));
        assert!(extract_note("聊天记录里的 #标签 不算").is_none());
    }

    #[test]
    fn bare_trigger_returns_empty_and_chat_is_untouched() {
        assert_eq!(extract_note("记一下").as_deref(), Some(""));
        // 句子中间出现触发词不捕获
        assert!(extract_note("你帮我记一下好不好").is_none());
        assert!(extract_note("老师提醒我们下周交报告").is_none());
        assert!(extract_note("今天天气不错").is_none());
        assert!(extract_note("/记一下 显式命令走 handle").is_none());
    }
}
