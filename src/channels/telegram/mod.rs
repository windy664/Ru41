//! Telegram 渠道：Bot API 适配器。
//!
//! 当前阶段只做**单向下发**（与飞书同级）：把终态结果、进度、活动流推到
//! Telegram 会话。收消息（长轮询网关）留到第二阶段。
//!
//! 协议细节见 `api.rs`，排版转换见 `markdown.rs`。

pub mod api;
pub mod bot;

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
    /// 每会话出站节流（限速冷却 + 最小间隔）；进程级共享。
    gate: &'static ChatGate,
    /// bot 用户名（正文深链需要它）：首次用到时用 getMe 取一次。
    bot_username: tokio::sync::OnceCell<String>,
}

/// 同一会话的最小发送间隔：Telegram 对单个会话的消息速率有限（实务上约 1 条/秒），
/// 长日报被切成多条后连发会被 429 拒收，消息直接丢。
const MIN_SEND_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1100);
/// 429 后就地退避的上限：超过就不等了，交给投递层改期重发
/// （网关是长驻进程，为一条消息睡几十秒会把其它会话一起堵住）。
const MAX_INLINE_FLOOD_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Telegram 深链：点击后 Telegram 会把 `/start <payload>` 发给机器人。
///
/// payload 用 base64url 编码命令文本——Telegram 的 start 参数只允许
/// `A-Za-z0-9_-`（且 ≤64 字符），中文命令必须编码。
pub fn deep_link(username: &str, command: &str) -> Option<String> {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(command.as_bytes());
    if payload.len() > MAX_START_PAYLOAD {
        return None;
    }
    Some(format!("tg://resolve?domain={username}&start={payload}"))
}

/// Telegram start 参数长度上限（官方 64 字符）。
pub const MAX_START_PAYLOAD: usize = 64;

/// 从深链 payload 解回命令文本（base64url）。
pub fn decode_start_payload(payload: &str) -> Option<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// 每会话的出站闸门：最小间隔 + 限速冷却截止时间。
#[derive(Default)]
struct ChatGate {
    min_interval: std::time::Duration,
    state: std::sync::Mutex<std::collections::HashMap<String, GateState>>,
}

#[derive(Default)]
struct GateState {
    /// 下一条消息允许发出的时刻。
    next_allowed: Option<std::time::Instant>,
    /// 平台限速的冷却截止时间。
    cooldown_until: Option<std::time::Instant>,
}

impl ChatGate {
    /// 排队等一个发送位；返回后该会话的间隔已预留（并发发送不会挤在一起）。
    async fn wait_turn(&self, chat_id: &str) {
        let wait = {
            let mut map = self.state.lock().unwrap();
            let now = std::time::Instant::now();
            let entry = map.entry(chat_id.to_owned()).or_default();
            let allowed = entry
                .next_allowed
                .into_iter()
                .chain(entry.cooldown_until)
                .max()
                .map(|at| at.max(now))
                .unwrap_or(now);
            entry.next_allowed = Some(allowed + self.min_interval);
            allowed.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    /// 平台限速：这段时间内该会话的所有出站都往后排。
    fn note_flood(&self, chat_id: &str, retry_after: std::time::Duration) {
        let mut map = self.state.lock().unwrap();
        let entry = map.entry(chat_id.to_owned()).or_default();
        let until = std::time::Instant::now() + retry_after;
        entry.cooldown_until = Some(
            entry
                .cooldown_until
                .map_or(until, |at: std::time::Instant| at.max(until)),
        );
    }
}

/// 进程级闸门：**网关回复**与**投递推送**是两条路，但发的是同一个会话——
/// 各自一份节流器就会各发各的，连发撞限速。所以进程内共用一份。
///
/// （跨进程不共享：生产里 Telegram 的主动推送也发生在另一个容器里。
/// 那种情况靠 429 分类退避兜底，不追求严格节奏。）
fn shared_gate() -> &'static ChatGate {
    static GATE: std::sync::OnceLock<ChatGate> = std::sync::OnceLock::new();
    GATE.get_or_init(|| ChatGate {
        min_interval: MIN_SEND_INTERVAL,
        state: std::sync::Mutex::new(std::collections::HashMap::new()),
    })
}

impl TelegramNotifier {
    pub fn new(api: Arc<TelegramApi>, default_chat_id: Option<String>) -> Self {
        Self {
            api,
            default_chat_id,
            gate: shared_gate(),
            bot_username: tokio::sync::OnceCell::new(),
        }
    }

    /// bot 用户名：正文深链要它（`tg://resolve?domain=…`）。取不到就退回纯符号。
    async fn bot_username(&self) -> Option<String> {
        let name = self
            .bot_username
            .get_or_init(|| async {
                self.api
                    .get_me()
                    .await
                    .map(|identity| identity.username)
                    .unwrap_or_default()
            })
            .await;
        (!name.is_empty()).then(|| name.clone())
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

    /// 正文渲染：可点指令占位符展开成**正文里的深链**（复选框本身可点）。
    ///
    /// 正文里只有链接能点（Telegram 没有「正文内嵌按钮」这种能力），所以用
    /// `tg://` 深链：点击后 Telegram 会把 `/start <payload>` 发给机器人。
    /// 底部按钮仍同时保留——那是「不切会话、一跳完成」的入口。
    async fn render_for_telegram(&self, chunk: &str) -> String {
        // 只有正文里真有可点指令时才去取 bot 用户名（getMe 是一次额外请求）。
        if !ru41_plugin_api::interactive::has_cmd_token(chunk) {
            return markdown::render(chunk, &markdown::TELEGRAM);
        }
        let Some(username) = self.bot_username().await else {
            return markdown::render(chunk, &markdown::TELEGRAM);
        };
        markdown::render_with_cmd_links(chunk, &markdown::TELEGRAM, &|command| {
            deep_link(&username, command)
        })
    }

    /// 正文深链是否已经覆盖了全部按钮动作 → 覆盖了就不要重复的底部键盘。
    ///
    /// 判定三件事：① 正文里确实有可点指令占位符（否则按钮不是重复入口）；
    /// ② 这些占位符**都**能变成深链（有一个造不出来就得留底部键盘兜底）；
    /// ③ 每个按钮的动作文本都在正文里出现过（正文没提到的额外动作不能被吞掉）。
    async fn text_links_cover_buttons(
        &self,
        text: &str,
        markup: &crate::ports::commands::ReplyMarkup,
    ) -> bool {
        let tokens: Vec<String> = ru41_plugin_api::interactive::cmd_tokens(text)
            .into_iter()
            .map(|(_, _, command)| command)
            .collect();
        if tokens.is_empty() {
            return false;
        }
        let Some(username) = self.bot_username().await else {
            return false;
        };
        if tokens
            .iter()
            .any(|command| deep_link(&username, command).is_none())
        {
            return false;
        }
        markup
            .rows
            .iter()
            .flatten()
            .all(|button| tokens.iter().any(|command| command == &button.command))
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
        let html = self.render_for_telegram(chunk).await;
        // 每会话排队：长日报会切成多条，连发会撞限速被 429 拒收（消息直接丢）。
        self.gate.wait_turn(chat_id).await;
        let result = self
            .api
            .send_message(chat_id, &html, Some("HTML"), reply_to, reply_markup)
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if api::is_parse_error(&error) => {
                // 排版被拒：降级成纯文本重发（仍要排队，别再撞一次限速）。
                self.gate.wait_turn(chat_id).await;
                self.api
                    .send_message(chat_id, chunk, None, reply_to, reply_markup)
                    .await?;
                Ok(())
            }
            Err(error) => {
                // 限速（429）：短等待就地退避重试；长等待不睡——交给投递层改期重发，
                // 长驻网关为一条消息睡几十秒会把其它会话一起堵住。
                if let Some(seconds) = api::flood_retry_after(&error) {
                    let wait = std::time::Duration::from_secs(seconds);
                    self.gate.note_flood(chat_id, wait);
                    if wait <= MAX_INLINE_FLOOD_WAIT {
                        tokio::time::sleep(wait).await;
                        self.gate.wait_turn(chat_id).await;
                        self.api
                            .send_message(chat_id, &html, Some("HTML"), reply_to, reply_markup)
                            .await?;
                        return Ok(());
                    }
                }
                Err(error)
            }
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
    ///
    /// 但**正文里已经是深链的可点指令不再重复挂底部键盘**——同一个动作两个入口
    /// 是噪音。只有深链造不出来（拿不到 bot 用户名、payload 超长）时，才退回
    /// 底部键盘，保证永远有一个可点入口。
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
            let keyboard = if self.text_links_cover_buttons(text, markup).await {
                None
            } else {
                inline_keyboard(markup)
            };
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
            // 媒体同样占会话的发送额度，多张图连发也要排队。
            self.gate.wait_turn(&chat_id).await;
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

    #[test]
    fn deep_link_round_trips_the_command() {
        let link = deep_link("Ru41bot", "/完成 abc123").expect("深链");
        assert!(link.starts_with("tg://resolve?domain=Ru41bot&start="), "{link}");
        let payload = link.split_once("start=").unwrap().1;
        // payload 是 base64url：只有 A-Za-z0-9_-（Telegram 的 start 参数约束）。
        assert!(
            payload
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{payload}"
        );
        assert_eq!(
            decode_start_payload(payload).as_deref(),
            Some("/完成 abc123")
        );
    }

    #[test]
    fn deep_link_gives_up_on_oversized_payload() {
        let long = format!("/完成 {}", "x".repeat(200));
        assert!(deep_link("Ru41bot", &long).is_none());
    }
}
