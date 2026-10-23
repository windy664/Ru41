pub mod api;
pub mod commands;
pub mod gateway;
pub mod onboard;

use crate::{
    app::task::TaskService,
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

/// 同一 msg_id 被动回复的序号器：msg_seq 必须从 1 开始递增且不能重复。
/// QQ 网关进程内 QqBot（命令/回执）与 QqNotifier（终态/进度通知）共享一个实例，
/// 避免同一 msg_id 下两条回复撞同一个 seq。
#[derive(Default)]
pub struct ReplySequencer {
    seq_by_msg: Mutex<HashMap<String, i64>>,
}

impl ReplySequencer {
    pub fn next(&self, msg_id: &str) -> i64 {
        let mut seqs = self.seq_by_msg.lock().unwrap_or_else(|e| e.into_inner());
        let seq = seqs.get(msg_id).copied().unwrap_or(0) + 1;
        seqs.insert(msg_id.to_owned(), seq);
        seq
    }
}

/// QQ 通知渠道：把终态结果作为被动回复发回原会话。
pub struct QqNotifier {
    api: Arc<QqApiClient>,
    sequencer: Arc<ReplySequencer>,
}

impl QqNotifier {
    pub fn new(api: Arc<QqApiClient>, sequencer: Arc<ReplySequencer>) -> Self {
        Self { api, sequencer }
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
            let seq = self.sequencer.next(key);
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
    /// 与 QqNotifier 共享的被动回复序号器，防止同 msg_id 下 seq 撞车。
    pub sequencer: Arc<ReplySequencer>,
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
        let (msg_id, content, target, author_openid, is_c2c, image_url) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                content,
                group_openid,
                author_openid,
                ..
            } => (
                msg_id,
                content,
                format!("{TARGET_GROUP_PREFIX}{group_openid}"),
                author_openid,
                false,
                None,
            ),
            GatewayEvent::C2cMessage {
                msg_id,
                content,
                user_openid,
                image_url,
                ..
            } => (
                msg_id,
                content,
                format!("{TARGET_C2C_PREFIX}{user_openid}"),
                user_openid,
                true,
                image_url.as_deref(),
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
            image_url,
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
        let receipt = self.ingest(event)?;
        // 只有发生打断才即时回复；正常入队不再发接单回执，
        // 用户直接看活动流（原生 codex 消息）即可。
        if let Some(text) = receipt.interrupt_text() {
            return Ok(DispatchOutcome::Reply(text.into()));
        }
        Ok(DispatchOutcome::Ingested)
    }

    /// 命令的即时被动回复；msg_seq 经共享序号器分配，与终态/进度通知不撞车。
    /// 有按钮走 markdown + keyboard 通道（平台不支持时发送层自动降级纯文本）。
    async fn reply(&self, target: &str, msg_id: &str, reply: &Reply) -> Result<()> {
        let seq = self.sequencer.next(msg_id);
        if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_group_markup(group, &reply.text, markup, Some(msg_id), seq)
                        .await?;
                }
                _ => {
                    self.api
                        .send_group_text(group, &reply.text, Some(msg_id), seq)
                        .await?;
                }
            }
        } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_c2c_markup(user, &reply.text, markup, Some(msg_id), seq)
                        .await?;
                }
                _ => {
                    self.api
                        .send_c2c_text(user, &reply.text, Some(msg_id), seq)
                        .await?;
                }
            }
        } else {
            bail!("unknown qq notify target: {target}")
        }
        Ok(())
    }

    /// 把一条消息事件幂等入队，并登记回话路由。
    /// 单聊新消息到达时，打断同会话未完成（排队/运行/等待重试）的旧任务：
    /// 旧任务的心跳下一轮即失败，agent 进程组被回收，新消息取代旧消息。
    /// 返回入执收据：任务 id、是否新创建、打断数量；调用方应立即回复 `receipt.notice()`。
    pub fn ingest(&self, event: &GatewayEvent) -> Result<IngestReceipt> {
        let (msg_id, content, target, private, attachments) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                content,
                group_openid,
                attachments,
                ..
            } => (
                msg_id,
                content,
                format!("{TARGET_GROUP_PREFIX}{group_openid}"),
                false,
                attachments,
            ),
            GatewayEvent::C2cMessage {
                msg_id,
                content,
                user_openid,
                attachments,
                ..
            } => (
                msg_id,
                content,
                format!("{TARGET_C2C_PREFIX}{user_openid}"),
                true,
                attachments,
            ),
            _ => bail!("unsupported gateway event for ingest"),
        };
        // 富媒体合成进 prompt：语音用平台 ASR 转写，文件给下载地址让 agent 自取。
        // 图片不走这里（image_url 有独立通道，由消费方处理）。
        let mut prompt = String::new();
        for attachment in attachments {
            match attachment.kind {
                gateway::AttachmentKind::Voice => {
                    if let Some(text) = &attachment.asr_text {
                        prompt.push_str(&format!("[语音转写] {text}\n"));
                    } else {
                        prompt.push_str(&format!(
                            "[语音] 平台未提供转写，WAV 地址：https://{}\n",
                            attachment.url
                        ));
                    }
                }
                gateway::AttachmentKind::File => {
                    let name = attachment.filename.as_deref().unwrap_or("未命名文件");
                    let size = attachment
                        .size
                        .map(|s| format!(" {} KB", s / 1024))
                        .unwrap_or_default();
                    prompt.push_str(&format!(
                        "[文件] {name}{size}\n下载地址（有时效，请尽快下载）：https://{}\n请下载后按我的要求处理。\n",
                        attachment.url
                    ));
                }
                gateway::AttachmentKind::Image => {}
            }
        }
        if !content.is_empty() {
            if !prompt.is_empty() {
                prompt.push('\n');
            }
            prompt.push_str(content);
        }
        // 幂等短路：同 msg_id 已入过队则直接回执原任务，不得先打断再判重
        // （否则平台重推同一条消息会反而把原任务取消掉）。
        let request_key = msg_request_key(msg_id);
        if let Some(existing) = self
            .store
            .list(200)?
            .into_iter()
            .find(|task| task.request_key == request_key)
        {
            return Ok(IngestReceipt {
                task_id: existing.id,
                created: false,
                interrupted: 0,
                ahead: 0,
            });
        }
        let mut interrupted = 0_usize;
        if private {
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
                match TaskService::new(self.store.clone()).cancel(task.id) {
                    Ok(_) => interrupted += 1,
                    Err(error) => {
                        eprintln!("[qq] interrupt task {} failed: {error:#}", task.id)
                    }
                }
            }
        }
        let enqueued = crate::app::task::TaskService::new(self.store.clone()).enqueue_with_notify(
            &request_key,
            &prompt,
            &self.backend,
            &NotifyRoute {
                channel: "qq".into(),
                target,
                msg_id: Some(msg_id.clone()),
            },
        )?;
        // 排队位置：除本任务外仍在排队/执行的任务数，让用户知道要等多久。
        let mut ahead = 0_usize;
        for task in self.store.list(50)? {
            if task.id == enqueued.task.id {
                continue;
            }
            if matches!(
                task.status,
                crate::domain::TaskStatus::Queued | crate::domain::TaskStatus::Running
            ) {
                ahead += 1;
            }
        }
        Ok(IngestReceipt {
            task_id: enqueued.task.id,
            created: enqueued.created,
            interrupted,
            ahead,
        })
    }
}

/// 消息入队后的回执：任务 id、是否新创建、打断数量。
/// 默认不回执（用户直接看活动流）；只有打断时才需要即时说明。
pub struct IngestReceipt {
    pub task_id: i64,
    pub created: bool,
    pub interrupted: usize,
    /// 除本任务外仍在排队/执行的任务数（保留供 CLI/HTTP 复用）。
    pub ahead: usize,
}

impl IngestReceipt {
    /// 发生打断时的即时提示；正常入队返回 None（不回执）。
    pub fn interrupt_text(&self) -> Option<String> {
        (self.interrupted > 0).then(|| {
            format!(
                "⏹ 已打断上一条未完成的任务（{} 个）",
                self.interrupted
            )
        })
    }
}
