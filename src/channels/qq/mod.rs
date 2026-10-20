pub mod api;
pub mod commands;
pub mod gateway;
pub mod onboard;

use crate::{
    domain::NotifyRoute,
    ports::{
        channel::NotifyChannel,
        commands::{CommandHandler, Reply},
        storage::TaskStore,
    },
};
use anyhow::{Result, bail};
use api::QqApiClient;
use gateway::{GatewayEvent, GatewaySession};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{mpsc, watch};

/// notify_target 的前缀：区分群会话与单聊。
pub const TARGET_GROUP_PREFIX: &str = "group:";
pub const TARGET_C2C_PREFIX: &str = "user:";

/// QQ 渠道的消息请求键，保证同一 msg_id 只入队一次。
pub fn msg_request_key(msg_id: &str) -> String {
    format!("qq:msg:{msg_id}")
}

/// QQ 通知渠道：把终态结果作为被动回复发回原会话。
pub struct QqNotifier {
    api: Arc<QqApiClient>,
    /// 同一 msg_id 多条回复需 msg_seq 自增。
    seq_by_msg: Mutex<HashMap<String, i64>>,
}

impl QqNotifier {
    pub fn new(api: Arc<QqApiClient>) -> Self {
        Self {
            api,
            seq_by_msg: Mutex::new(HashMap::new()),
        }
    }
}

impl NotifyChannel for QqNotifier {
    fn channel_id(&self) -> &str {
        "qq"
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let key = msg_id.unwrap_or_default();
            let seq = {
                let mut seqs = self
                    .seq_by_msg
                    .lock()
                    .map_err(|_| anyhow::anyhow!("qq seq lock poisoned"))?;
                let seq = seqs.get(key).copied().unwrap_or(0) + 1;
                seqs.insert(key.to_owned(), seq);
                seq
            };
            if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
                self.api.send_group_text(group, text, msg_id, seq).await?;
            } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
                self.api.send_c2c_text(user, text, msg_id, seq).await?;
            } else {
                bail!("unknown qq notify target: {target}");
            }
            Ok(())
        })
    }
}

/// QQ 网关：收消息幂等入队任务；固定命令立即回复，终态回复由 runtime::dispatcher 统一负责。
pub struct QqBot {
    pub api: Arc<QqApiClient>,
    pub store: Arc<dyn TaskStore>,
    pub backend: String,
    /// 身份绑定口令；None 时绑定功能关闭。
    pub bind_secret: Option<String>,
    /// 业务模块注册的扩展命令；QQ 渠道不直接依赖业务模块。
    pub command_handlers: Vec<Arc<dyn CommandHandler>>,
}

/// 单条消息的分发结果：立即回复（含可选按钮），或已幂等入队。
enum DispatchOutcome {
    Reply(Reply),
    Ingested,
}

impl QqBot {
    /// 长驻运行 gateway 事件循环，收到 shutdown 后优雅退出。
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let (events, mut receiver) = mpsc::channel::<GatewayEvent>(256);
        let gateway = self.gateway_loop(events, shutdown.clone());
        let ingest = async {
            while let Some(event) = receiver.recv().await {
                if let Err(error) = self.handle(&event).await {
                    eprintln!("[qq] handle failed: {error:#}");
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        // 任一分支结束即整体退出，其余 future 随 select 取消。
        tokio::select! {
            biased;
            _ = shutdown.changed() => {}
            result = gateway => result?,
            result = ingest => result?,
        }
        Ok(())
    }

    /// 断线重连：能 RESUME 就 RESUME，fatal 关闭码直接退出。
    async fn gateway_loop(
        &self,
        events: mpsc::Sender<GatewayEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut session = GatewaySession::default();
        let mut retries = 0_u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let result = async {
                let token = self.api.access_token().await?;
                let url = self.api.gateway_url().await?;
                gateway::run(&url, &token, &mut session, events.clone(), shutdown.clone()).await
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    retries += 1;
                    eprintln!("[qq] gateway disconnected: {error:#}");
                    let delay = Duration::from_secs(u64::from(1_u32 << retries.min(5)).min(30));
                    tokio::select! {
                        _ = shutdown.changed() => return Ok(()),
                        _ = tokio::time::sleep(delay) => {}
                    }
                }
            }
        }
    }

    /// 内置命令与注册的扩展命令立即执行并回复；未命中的消息走幂等入队。
    /// 命令执行与入队是同步存储 I/O，经 block_in_place 让出 executor 线程
    /// （与 HTTP/consumer 的 spawn_blocking 同一纪律）；扩展命令内部的
    /// 真实 async（如 /精读 拉取 arXiv）由 Handle::block_on 驱动。
    async fn handle(&self, event: &GatewayEvent) -> Result<()> {
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.dispatch(event))
        })?;
        let DispatchOutcome::Reply(reply) = outcome else {
            return Ok(());
        };
        let (msg_id, target) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                group_openid,
                ..
            } => (msg_id, format!("{TARGET_GROUP_PREFIX}{group_openid}")),
            GatewayEvent::C2cMessage {
                msg_id,
                user_openid,
                ..
            } => (msg_id, format!("{TARGET_C2C_PREFIX}{user_openid}")),
            _ => return Ok(()),
        };
        self.reply(&target, msg_id, &reply).await
    }

    /// 命令分发与消息入队；同步存储 I/O 全部在此路径上（调用方已做 block_in_place）。
    async fn dispatch(&self, event: &GatewayEvent) -> Result<DispatchOutcome> {
        let (msg_id, content, target, author_openid, is_c2c) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                content,
                group_openid,
                author_openid,
            } => (
                msg_id,
                content,
                format!("{TARGET_GROUP_PREFIX}{group_openid}"),
                author_openid,
                false,
            ),
            GatewayEvent::C2cMessage {
                msg_id,
                content,
                user_openid,
            } => (
                msg_id,
                content,
                format!("{TARGET_C2C_PREFIX}{user_openid}"),
                user_openid,
                true,
            ),
            _ => return Ok(DispatchOutcome::Ingested),
        };
        let ctx = crate::ports::commands::CommandContext {
            store: &self.store,
            bind_secret: self.bind_secret.as_deref(),
            target: &target,
            msg_id,
            author_openid,
            is_c2c,
            reply_channel: "qq",
        };
        if let Some(command) = commands::parse_command(content) {
            let reply = if command == commands::Command::Help {
                // 帮助文本需汇总扩展命令，单独构造；按钮与 /帮助 execute 一致。
                let mut reply = commands::execute(&commands::Command::Help, &ctx)?;
                reply.text = commands::help_text(&self.command_handlers);
                reply
            } else {
                commands::execute(&command, &ctx)?
            };
            return Ok(DispatchOutcome::Reply(reply));
        }
        let mut parts = content.trim().splitn(2, char::is_whitespace);
        let verb = parts.next().unwrap_or_default().to_lowercase();
        let verb = verb.strip_prefix('/').unwrap_or(&verb);
        let arg = parts.next().map(str::trim).unwrap_or_default();
        for handler in &self.command_handlers {
            if handler.matches(verb)
                && let Some(reply) = handler.handle_command(verb, arg, &ctx).await?
            {
                return Ok(DispatchOutcome::Reply(reply));
            }
        }
        for handler in &self.command_handlers {
            if let Some(reply) = handler.handle_message(content, &ctx).await? {
                return Ok(DispatchOutcome::Reply(reply));
            }
        }
        self.ingest(event)?;
        Ok(DispatchOutcome::Ingested)
    }

    /// 命令的即时被动回复；每条命令消息只回一条，msg_seq 固定为 1。
    /// 有按钮走 markdown + keyboard 通道（平台不支持时发送层自动降级纯文本）。
    async fn reply(&self, target: &str, msg_id: &str, reply: &Reply) -> Result<()> {
        if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_group_markup(group, &reply.text, markup, Some(msg_id), 1)
                        .await?;
                }
                _ => {
                    self.api
                        .send_group_text(group, &reply.text, Some(msg_id), 1)
                        .await?;
                }
            }
        } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_c2c_markup(user, &reply.text, markup, Some(msg_id), 1)
                        .await?;
                }
                _ => {
                    self.api
                        .send_c2c_text(user, &reply.text, Some(msg_id), 1)
                        .await?;
                }
            }
        } else {
            bail!("unknown qq notify target: {target}")
        }
        Ok(())
    }

    /// 把一条消息事件幂等入队，并登记回话路由。
    pub fn ingest(&self, event: &GatewayEvent) -> Result<()> {
        let (msg_id, content, target) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                content,
                group_openid,
                ..
            } => (
                msg_id,
                content,
                format!("{TARGET_GROUP_PREFIX}{group_openid}"),
            ),
            GatewayEvent::C2cMessage {
                msg_id,
                content,
                user_openid,
            } => (msg_id, content, format!("{TARGET_C2C_PREFIX}{user_openid}")),
            _ => return Ok(()),
        };
        crate::app::task::TaskService::new(self.store.clone()).enqueue_with_notify(
            &msg_request_key(msg_id),
            content,
            &self.backend,
            &NotifyRoute {
                channel: "qq".into(),
                target,
                msg_id: Some(msg_id.clone()),
            },
        )?;
        Ok(())
    }
}
