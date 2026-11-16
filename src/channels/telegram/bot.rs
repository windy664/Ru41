//! Telegram 网关：长轮询收消息 → 内置/扩展命令 → 自然语言入队 → 回复。
//!
//! 协议要点：
//! - `getUpdates` 长轮询，offset = 已处理 update_id + 1；同一 token 只能有一个消费者；
//! - 入站消息带 `message_id`（整数）与 `chat.id`，前者可作 `reply_parameters`（Telegram
//!   没有 QQ 那样的被动回复窗口，这个 id 长期有效）；
//! - 群里默认只有命令/提及/回复才推给 bot（隐私模式），私聊消息始终可达；
//! - 按钮点击走 `callback_query`：应答后按按钮里的指令文本执行，等价于「代用户发消息」。

use crate::{
    app::task::TaskService,
    channels::builtin,
    channels::telegram::{TelegramNotifier, api::TelegramApi},
    domain::NotifyRoute,
    ports::{
        channel::NotifyChannel,
        commands::{CommandContext, CommandHandler, Reply},
        storage::TaskStore,
    },
};
use anyhow::Result;
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// getUpdates 的服务端等待秒数；客户端总超时更长（见 `TelegramApi` 的 90s）。
const POLL_TIMEOUT_SECS: u64 = 20;
/// 每 N 轮查一次平台侧待处理条数（轮询存活探针）。
const PROBE_EVERY: u32 = 3;
/// offset 持久化的文档位置（重启后从上次继续，不重复消费）。
const OFFSET_KIND: &str = "telegram";
const OFFSET_ID: &str = "offset";
/// 长轮询超过这个时长仍未返回 → 视为卡死，重启这一轮。
const STUCK_AFTER: Duration = Duration::from_secs(POLL_TIMEOUT_SECS + 45);

/// 一条入站消息（只取网关需要的字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMessage {
    pub update_id: i64,
    pub message_id: i64,
    pub chat_id: i64,
    /// `private` / `group` / `supergroup` / `channel`。
    pub chat_type: String,
    pub user_id: i64,
    pub text: String,
}

impl InboundMessage {
    /// 私聊才算「单聊」：私人数据（待办、水电）只在单聊且已绑定时展示。
    pub fn is_private(&self) -> bool {
        self.chat_type == "private"
    }

    /// notify target：`chat:{chat_id}`，与 QQ 的 `user:{openid}` 同构。
    pub fn target(&self) -> String {
        format!("chat:{}", self.chat_id)
    }
}

/// Telegram 网关。
pub struct TelegramBot {
    pub api: Arc<TelegramApi>,
    pub notifier: TelegramNotifier,
    pub store: Arc<dyn TaskStore>,
    /// 消息任务入队时绑定的底座。
    pub backend: String,
    /// 身份绑定口令；None 时绑定功能关闭。
    pub bind_secret: Option<String>,
    /// 业务模块注册的扩展命令（与渠道无关：模块内用 `ctx.reply_channel`）。
    pub command_handlers: Vec<Arc<dyn CommandHandler>>,
    /// 用户白名单（Telegram user id）；为空表示不限制。
    pub allowed_user_ids: Vec<i64>,
    pub pricing: crate::app::usage_report::Pricing,
}

impl TelegramBot {
    /// 长驻运行消息网关，直到收到 shutdown。
    pub async fn run(self: &Arc<Self>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut offset = self.load_offset();
        eprintln!("[telegram] 网关启动：offset={offset}");
        let mut backoff = 1_u64;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            match self.poll_once(&mut offset, &mut shutdown).await {
                Ok(()) => backoff = 1,
                Err(error) => {
                    eprintln!("[telegram] 轮询失败：{error:#}");
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(Duration::from_secs(backoff.min(30))) => {}
                    }
                    backoff = backoff.saturating_mul(2);
                }
            }
        }
    }

    /// 一轮长轮询循环：反复 getUpdates 直到出错或收到 shutdown。
    async fn poll_once(
        &self,
        offset: &mut i64,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let mut rounds = 0_u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            rounds += 1;
            let started = Instant::now();
            let updates = self.api.get_updates(*offset, POLL_TIMEOUT_SECS).await?;
            let elapsed = started.elapsed();
            if elapsed > STUCK_AFTER {
                // 长轮询卡死（socket 半开）：放弃这一轮，重新发起。
                eprintln!("[telegram] 长轮询耗时 {elapsed:?}，判定卡死，重启轮询");
                return Ok(());
            }
            let received = updates.len();
            for update in &updates {
                let id = update["update_id"].as_i64().unwrap_or(*offset);
                self.handle_update(update).await;
                *offset = id + 1;
            }
            if received > 0 {
                eprintln!("[telegram] 收到 {received} 条更新，新 offset={offset}");
                self.save_offset(*offset)?;
            }
            // 存活探针：平台侧有排队但我们这轮没收到 → 消费者可能卡住。
            if rounds.is_multiple_of(PROBE_EVERY) && received == 0 {
                let pending = self.api.pending_update_count().await.unwrap_or(0);
                if pending > 0 {
                    anyhow::bail!("平台侧排队 {pending} 条但本轮未收到，长轮询疑似卡死");
                }
            }
        }
    }

    /// 处理一条更新：消息走命令/入队，按钮走回调。
    /// 公开以便集成测试直接驱动（不依赖长轮询时序）。
    pub async fn handle_update(&self, update: &Value) {
        if let Some(callback) = update.get("callback_query") {
            self.handle_callback(callback).await;
            return;
        }
        let Some(message) = parse_message(update) else {
            return;
        };
        if !self.allowed(message.user_id) {
            eprintln!("[telegram] 忽略非白名单用户 {}", message.user_id);
            return;
        }
        let target = message.target();
        let author = message.user_id.to_string();
        let msg_id = message.message_id.to_string();
        let ctx = CommandContext {
            store: &self.store,
            bind_secret: self.bind_secret.as_deref(),
            target: &target,
            msg_id: &msg_id,
            author_openid: &author,
            is_c2c: message.is_private(),
            image_url: None,
            attachments: &[],
            reply_channel: "telegram",
        };
        // 深链：正文里的复选框点开 → Telegram 发来 `/start <payload>`，
        // payload 里是编码过的命令文本，按用户输入的命令执行（身份/私聊闸门照旧）。
        if let Some(command_text) = start_payload(&message.text) {
            match self.run_command_text(&command_text, &ctx).await {
                Ok(Some(reply)) => self.reply(&target, message.message_id, &reply).await,
                Ok(None) => eprintln!("[telegram] 深链命令无法识别：{command_text}"),
                Err(error) => eprintln!("[telegram] 深链命令执行失败：{error:#}"),
            }
            return;
        }
        // 1) 内置命令
        if let Some(command) = builtin::parse_command(&message.text, &builtin::DEFAULT) {
            match builtin::execute(&command, &ctx, &self.pricing, &builtin::DEFAULT) {
                Ok(reply) => self.reply(&target, message.message_id, &reply).await,
                Err(error) => {
                    eprintln!("[telegram] 内置命令执行失败：{error:#}");
                }
            }
            return;
        }
        // 2) 扩展命令（插件注册）
        if let Some(reply) = self.run_extension(&message.text, &ctx).await {
            self.reply(&target, message.message_id, &reply).await;
            return;
        }
        // 3) Telegram 的 /start 是平台约定：回一句欢迎，不入队。
        if is_start_command(&message.text) {
            self.reply(
                &target,
                message.message_id,
                &Reply::text(format!(
                    "## Ru41 已就绪\n\n直接发消息即可（自然语言会交给 Agent）；命令见 `{}`。",
                    builtin::DEFAULT.shown.help
                )),
            )
            .await;
            return;
        }
        // 3) 自然语言：入队给 Agent。
        if message.text.trim().is_empty() {
            self.reply(
                &target,
                message.message_id,
                &Reply::text("## 收到非文本消息\n\n暂只支持文本与命令；媒体接入待后续版本。"),
            )
            .await;
            return;
        }
        if let Err(error) = self.ingest(&message).await {
            eprintln!("[telegram] 入队失败：{error:#}");
        }
    }

    /// 执行一条命令文本：内置命令优先，再落到扩展命令（插件）。
    /// 不是命令则返回 None（交给自然语言）。
    async fn run_command_text(
        &self,
        text: &str,
        ctx: &CommandContext<'_>,
    ) -> Result<Option<Reply>> {
        if let Some(command) = builtin::parse_command(text, &builtin::DEFAULT) {
            return builtin::execute(&command, ctx, &self.pricing, &builtin::DEFAULT).map(Some);
        }
        Ok(self.run_extension(text, ctx).await)
    }

    /// 扩展命令：先按动词匹配，再让模块自行判定自然语言。
    async fn run_extension(&self, content: &str, ctx: &CommandContext<'_>) -> Option<Reply> {
        let mut parts = content.trim().splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or_default().to_lowercase();
        let verb = verb.strip_prefix('/').unwrap_or(&verb).to_owned();
        let arg = parts.next().map(str::trim).unwrap_or_default();
        for handler in &self.command_handlers {
            if handler.matches(&verb)
                && let Some(reply) = handler.handle_command(&verb, arg, ctx).await.ok().flatten()
            {
                return Some(reply);
            }
        }
        for handler in &self.command_handlers {
            if let Ok(Some(reply)) = handler.handle_message(content, ctx).await {
                return Some(reply);
            }
        }
        None
    }

    /// 按钮回调：先应答，再把按钮里的指令文本当作用户发的消息执行。
    async fn handle_callback(&self, callback: &Value) {
        let id = callback["id"].as_str().unwrap_or_default().to_owned();
        let data = callback["data"].as_str().unwrap_or_default().to_owned();
        let chat_id = callback["message"]["chat"]["id"]
            .as_i64()
            .unwrap_or_default();
        let user_id = callback["from"]["id"].as_i64().unwrap_or_default();
        if !id.is_empty()
            && let Err(error) = self.api.answer_callback_query(&id).await
        {
            eprintln!("[telegram] 按钮应答失败：{error:#}");
        }
        if data.is_empty() || chat_id == 0 {
            return;
        }
        if !self.allowed(user_id) {
            return;
        }
        let target = format!("chat:{chat_id}");
        let author = user_id.to_string();
        let msg_id = callback["message"]["message_id"]
            .as_i64()
            .unwrap_or_default()
            .to_string();
        // 私聊判定看会话类型：群里的按钮同样属于群聊，私人数据闸门不能被绕过。
        let chat_type = callback["message"]["chat"]["type"]
            .as_str()
            .unwrap_or("private");
        let ctx = CommandContext {
            store: &self.store,
            bind_secret: self.bind_secret.as_deref(),
            target: &target,
            msg_id: &msg_id,
            author_openid: &author,
            is_c2c: chat_type == "private",
            image_url: None,
            attachments: &[],
            reply_channel: "telegram",
        };
        let reply = match builtin::parse_command(&data, &builtin::DEFAULT) {
            Some(command) => {
                builtin::execute(&command, &ctx, &self.pricing, &builtin::DEFAULT).ok()
            }
            None => self.run_extension(&data, &ctx).await,
        };
        if let Some(reply) = reply {
            self.reply(&target, 0, &reply).await;
        }
    }

    /// 回一条消息（带按钮时用 inline keyboard）。
    async fn reply(&self, target: &str, message_id: i64, reply: &Reply) {
        let msg_id = (message_id > 0).then(|| message_id.to_string());
        let result = match &reply.markup {
            Some(markup) if !markup.is_empty() => {
                self.notifier
                    .send_with_markup(target, msg_id.as_deref(), &reply.text, markup)
                    .await
            }
            _ => {
                self.notifier
                    .send(target, msg_id.as_deref(), &reply.text)
                    .await
            }
        };
        if let Err(error) = result {
            eprintln!("[telegram] 回复失败：{error:#}");
        }
    }

    /// 自然语言入队：同会话同 message_id 幂等；私聊新消息打断同会话未完成的旧任务。
    async fn ingest(&self, message: &InboundMessage) -> Result<()> {
        // message_id **只在会话内唯一**：群聊与私聊可以是同一个号，键里必须带
        // chat，否则两个会话的消息会互相顶掉（后一条被当成重复而丢弃）。
        let request_key = crate::ports::commands::msg_request_key(
            "telegram",
            &format!("{}:{}", message.chat_id, message.message_id),
        );
        if let Some(existing) = self
            .store
            .list(200)?
            .into_iter()
            .find(|task| task.request_key == request_key)
        {
            eprintln!("[telegram] 重复消息已忽略（任务 #{}）", existing.id);
            return Ok(());
        }
        let target = message.target();
        if message.is_private() {
            for task in self.store.list(50)? {
                if !matches!(
                    task.status,
                    crate::domain::TaskStatus::Queued
                        | crate::domain::TaskStatus::Running
                        | crate::domain::TaskStatus::RetryScheduled
                ) {
                    continue;
                }
                if task.notify.as_ref().map(|route| route.target.as_str()) != Some(target.as_str())
                {
                    continue;
                }
                if let Err(error) = TaskService::new(self.store.clone()).cancel(task.id) {
                    eprintln!("[telegram] 打断任务 {} 失败：{error:#}", task.id);
                }
            }
        }
        let enqueued = TaskService::new(self.store.clone()).enqueue_with_notify(
            &request_key,
            &message.text,
            &self.backend,
            &NotifyRoute {
                channel: "telegram".into(),
                target,
                msg_id: Some(message.message_id.to_string()),
            },
        );
        match enqueued {
            Ok(_) => {
                eprintln!("[telegram] 已入队：message_id={}", message.message_id);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// 用户白名单：空列表表示不限制。
    fn allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&user_id)
    }

    /// 读回上次处理到的位置（下次 getUpdates 要用的 offset）。
    fn load_offset(&self) -> i64 {
        self.store
            .document(OFFSET_KIND, OFFSET_ID)
            .ok()
            .flatten()
            .and_then(|doc| doc.value["offset"].as_i64())
            .unwrap_or(0)
    }

    /// 持久化 offset：重启后从上次继续，不重复消费也不漏。
    fn save_offset(&self, offset: i64) -> Result<()> {
        let current = self.store.document(OFFSET_KIND, OFFSET_ID)?;
        let revision = current.as_ref().map(|doc| doc.revision);
        self.store.put_document(
            OFFSET_KIND,
            OFFSET_ID,
            revision,
            &serde_json::json!({"offset": offset}),
        )?;
        Ok(())
    }
}

/// Telegram 的 `/start`（可带 `@botname` 后缀）：平台约定，不当作任务。
fn is_start_command(text: &str) -> bool {
    let text = text.trim();
    let text = text.strip_prefix('/').unwrap_or(text);
    text == "start" || text.starts_with("start@")
}

/// `/start <payload>`：深链带来的命令。
///
/// payload 是 base64url 编码的命令文本（`channels::telegram::deep_link` 生成）。
/// 解不出、或解出来不是命令（不以 `/` 开头）→ 当作普通 `/start` 处理。
fn start_payload(text: &str) -> Option<String> {
    let text = text.trim();
    let payload = if let Some(rest) = text.strip_prefix("/start ") {
        rest
    } else {
        // "/start@bot payload"：前段是 bot 用户名。
        let rest = text.strip_prefix("/start@")?;
        let (_, payload) = rest.split_once(' ')?;
        payload
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }
    let decoded = crate::channels::telegram::decode_start_payload(payload)?;
    decoded.starts_with('/').then_some(decoded)
}

/// 从更新对象里取消息；非文本消息也会被解析出来（文本为空）。
pub fn parse_message(update: &Value) -> Option<InboundMessage> {
    let update_id = update["update_id"].as_i64()?;
    let message = update.get("message")?;
    let chat_id = message["chat"]["id"].as_i64()?;
    let message_id = message["message_id"].as_i64()?;
    let chat_type = message["chat"]["type"]
        .as_str()
        .unwrap_or("private")
        .to_owned();
    let user_id = message["from"]["id"].as_i64().unwrap_or_default();
    let text = message["text"].as_str().unwrap_or_default().to_owned();
    Some(InboundMessage {
        update_id,
        message_id,
        chat_id,
        chat_type,
        user_id,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_message() {
        let update = serde_json::json!({
            "update_id": 42,
            "message": {
                "message_id": 7,
                "chat": {"id": 12345, "type": "private"},
                "from": {"id": 678},
                "text": "hello"
            }
        });
        let message = parse_message(&update).unwrap();
        assert_eq!(message.update_id, 42);
        assert_eq!(message.message_id, 7);
        assert_eq!(message.chat_id, 12345);
        assert_eq!(message.user_id, 678);
        assert_eq!(message.text, "hello");
        assert!(message.is_private());
        assert_eq!(message.target(), "chat:12345");
    }

    #[test]
    fn non_message_update_is_ignored() {
        let update = serde_json::json!({"update_id": 1, "callback_query": {"id": "x"}});
        assert!(parse_message(&update).is_none());
    }
}
