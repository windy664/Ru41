//! Telegram 渠道：Bot API 适配器。
//!
//! 当前阶段只做**单向下发**（与飞书同级）：把终态结果、进度、活动流推到
//! Telegram 会话。收消息（长轮询网关）留到第二阶段。
//!
//! 协议细节见 `api.rs`，排版转换见 `markdown.rs`。

pub mod api;
pub mod bot;
pub mod commands;

use crate::{
    channels::markdown,
    ports::channel::{MediaRef, NotifyChannel},
};
use anyhow::{Context, Result, bail};
use std::{future::Future, pin::Pin, sync::Arc};

pub use api::{BotIdentity, TelegramApi};

/// notify_target 的占位值：真正的 chat_id 由配置提供，不落库。
pub const DEFAULT_TARGET: &str = "default";
/// notify_target 前缀：`chat:{id}` 明确指定会话（数字 chat_id 或 @username）。
pub const TARGET_CHAT_PREFIX: &str = "chat:";

/// Telegram 通知渠道：把结果发到指定会话。
pub struct TelegramNotifier {
    api: Arc<TelegramApi>,
    /// target 为 `default` 时使用的默认 chat（数字 chat_id 或 @username）。
    default_chat_id: Option<String>,
}

impl TelegramNotifier {
    pub fn new(api: Arc<TelegramApi>, default_chat_id: Option<String>) -> Self {
        Self {
            api,
            default_chat_id,
        }
    }

    /// 解析 target：`default` 用配置值；`chat:{id}`、裸数字或 @username 直接用。
    fn resolve_chat(&self, target: &str) -> Result<String> {
        let target = target.trim();
        if target == DEFAULT_TARGET {
            return self
                .default_chat_id
                .as_deref()
                .map(str::trim)
                .filter(|chat| !chat.is_empty())
                .map(str::to_owned)
                .context(
                    "telegram default chat is not configured (channel.telegram.default_chat_id)",
                );
        }
        if let Some(chat) = target.strip_prefix(TARGET_CHAT_PREFIX) {
            let chat = chat.trim();
            if chat.is_empty() {
                bail!("empty telegram chat id in notify target");
            }
            return Ok(chat.to_owned());
        }
        if target.starts_with('@') || target.chars().all(|c| c.is_ascii_digit()) {
            return Ok(target.to_owned());
        }
        bail!("unknown telegram notify target: {target}");
    }

    /// 发一条（已分片）消息；排版被平台拒收时降级成纯文本重发一次。
    async fn deliver(
        &self,
        chat_id: &str,
        chunk: &str,
        reply_to: Option<i64>,
        reply_markup: Option<&serde_json::Value>,
    ) -> Result<()> {
        // 通用渲染层：规范 Markdown → Telegram HTML（表格/公式统一降级）。
        let html = markdown::render(chunk, &markdown::TELEGRAM);
        match self
            .api
            .send_message(chat_id, &html, Some("HTML"), reply_to, reply_markup)
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if api::is_parse_error(&error) => {
                self.api
                    .send_message(chat_id, chunk, None, reply_to, reply_markup)
                    .await?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}

impl NotifyChannel for TelegramNotifier {
    fn channel_id(&self) -> &str {
        "telegram"
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let chat_id = self.resolve_chat(target)?;
            let reply_to = msg_id.and_then(|id| id.parse::<i64>().ok());
            for chunk in markdown::split_markdown(text, markdown::CHUNK_BUDGET) {
                self.deliver(&chat_id, &chunk, reply_to, None).await?;
            }
            Ok(())
        })
    }

    /// 带快捷按钮的通知：ReplyMarkup → Telegram inline keyboard。
    fn send_with_markup<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
        markup: &'a crate::ports::commands::ReplyMarkup,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let chat_id = self.resolve_chat(target)?;
            let reply_to = msg_id.and_then(|id| id.parse::<i64>().ok());
            let keyboard = inline_keyboard(markup);
            let chunks = markdown::split_markdown(text, markdown::CHUNK_BUDGET);
            let last = chunks.len().saturating_sub(1);
            for (index, chunk) in chunks.iter().enumerate() {
                let markup = if index == last {
                    keyboard.as_ref()
                } else {
                    None
                };
                self.deliver(&chat_id, chunk, reply_to, markup).await?;
            }
            Ok(())
        })
    }

    fn send_media<'a>(
        &'a self,
        target: &'a str,
        _msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let chat_id = self.resolve_chat(target)?;
            self.api.send_media(&chat_id, media, caption).await?;
            Ok(())
        })
    }
}

/// ReplyMarkup → Telegram inline keyboard；callback_data 上限 64 字节。
fn inline_keyboard(markup: &crate::ports::commands::ReplyMarkup) -> Option<serde_json::Value> {
    let rows: Vec<serde_json::Value> = markup
        .rows
        .iter()
        .filter(|row| !row.is_empty())
        .map(|row| {
            serde_json::Value::Array(
                row.iter()
                    .map(|button| {
                        serde_json::json!({
                            "text": button.label,
                            "callback_data": markdown::truncate_bytes(&button.command, 64),
                        })
                    })
                    .collect(),
            )
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(serde_json::json!({"inline_keyboard": rows}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notifier(default_chat_id: Option<&str>) -> TelegramNotifier {
        TelegramNotifier::new(
            Arc::new(TelegramApi::new("123456:token")),
            default_chat_id.map(str::to_owned),
        )
    }

    #[test]
    fn resolves_default_chat_from_config() {
        let notifier = notifier(Some("998877"));
        assert_eq!(notifier.resolve_chat("default").unwrap(), "998877");
    }

    #[test]
    fn resolves_explicit_chat_targets() {
        let notifier = notifier(Some("998877"));
        assert_eq!(notifier.resolve_chat("chat:123").unwrap(), "123");
        assert_eq!(notifier.resolve_chat("456").unwrap(), "456");
        assert_eq!(notifier.resolve_chat("@ru41bot").unwrap(), "@ru41bot");
    }

    #[test]
    fn rejects_unknown_target_and_missing_default() {
        let notifier = notifier(None);
        assert!(notifier.resolve_chat("default").is_err());
        assert!(notifier.resolve_chat("not-a-chat").is_err());
    }

    #[test]
    fn builds_inline_keyboard_with_truncated_callback_data() {
        let markup = crate::ports::commands::ReplyMarkup::new().row(vec![
            crate::ports::commands::MarkupButton::new("取消", format!("/取消 {}", "x".repeat(100))),
        ]);
        let keyboard = inline_keyboard(&markup).unwrap();
        let buttons = &keyboard["inline_keyboard"][0];
        assert_eq!(buttons[0]["text"], "取消");
        assert!(buttons[0]["callback_data"].as_str().unwrap().len() <= 64);
    }
}
