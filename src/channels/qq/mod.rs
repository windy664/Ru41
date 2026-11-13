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
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

/// notify_target 的前缀：区分群会话与单聊。
pub const TARGET_GROUP_PREFIX: &str = "group:";
pub const TARGET_C2C_PREFIX: &str = "user:";

/// QQ 渠道的消息请求键，保证同一 msg_id 只入队一次。
/// 实现已上移到 `ports::commands::msg_request_key`（模块侧也复用），此处 re-export 兼容旧路径。
pub use crate::ports::commands::msg_request_key;

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

    /// 带快捷按钮的通知（markdown + keyboard，失败按 无键盘→纯文本 降级）。
    fn send_with_markup<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
        markup: &'a crate::ports::commands::ReplyMarkup,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let key = msg_id.unwrap_or_default();
            let seq = self.sequencer.next(key);
            if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
                self.api
                    .send_group_markup(group, text, markup, msg_id, seq)
                    .await?;
            } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
                self.api
                    .send_c2c_markup(user, text, markup, msg_id, seq)
                    .await?;
            } else {
                bail!("unknown qq notify target: {target}");
            }
            Ok(())
        })
    }

    fn send_media<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        caption: &'a str,
        media: &'a crate::ports::channel::MediaRef,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let key = msg_id.unwrap_or_default();
            let seq = self.sequencer.next(key);
            if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
                self.api
                    .send_group_media(group, caption, media, msg_id, seq)
                    .await?;
            } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
                self.api
                    .send_c2c_media(user, caption, media, msg_id, seq)
                    .await?;
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
    /// 业务模块注册的图片消费者（如学习通二维码照片签到）；同样不感知具体业务。
    pub image_consumers: Vec<Arc<dyn crate::ports::commands::ImageConsumer>>,
    /// 与 QqNotifier 共享的被动回复序号器，防止同 msg_id 下 seq 撞车。
    pub sequencer: Arc<ReplySequencer>,
    /// 合并排水状态（忙会话的连续消息缓冲）。
    pub coalesce: Arc<Mutex<CoalesceState>>,
    /// 「正在输入」指示的发送时间（按 user_openid 去抖，平台指示有效 60s）。
    pub typing_sent_at: Mutex<HashMap<String, Instant>>,
    /// 供应商定价：用量报告折算「实际花费」用。
    pub pricing: crate::app::usage_report::Pricing,
}

/// 单条消息的分发结果：立即回复（含可选按钮），或已幂等入队。
enum DispatchOutcome {
    Reply(Reply),
    Ingested,
}

/// 合并排水（参考 OpenMausBot admission.ts 的 drain coalescing）：
/// 会话「忙」（有排队/运行中的任务）时，续发的自然语言消息不再立即
/// 入队打断前一条，而是攒进缓冲，静默窗口结束后合并成一个任务——
/// 连发消息从「互相打断、上下文割裂」变成「一轮答完」。
/// 闲会话不延迟：第一条消息仍然即时入队。
#[derive(Default)]
pub struct CoalesceState {
    /// key = "{target}|{author_openid}"（群聊按人分缓冲，单人话题不串）。
    buffers: HashMap<String, CoalesceBuffer>,
}

#[derive(Default)]
struct CoalesceBuffer {
    /// 代数：每来一条新消息 +1；过期的 flush 任务看到代数变了就让位。
    generation: u64,
    items: Vec<CoalesceItem>,
    /// 首条入缓冲时间：封顶窗口，连续输入也不无限顺延。
    first_at: Option<Instant>,
    is_c2c: bool,
}

struct CoalesceItem {
    msg_id: String,
    /// 已渲染的 prompt 片段（附件说明 + 正文）。
    prompt: String,
}

/// 静默窗口：最后一条消息到达后这么久没有续杯就合并入队。
const COALESCE_DEBOUNCE: Duration = Duration::from_secs(12);
/// 首条入缓冲后的最长等待：连续输入最多顺延到这个上限。
const COALESCE_MAX_WAIT: Duration = Duration::from_secs(60);
/// 缓冲条数上限：到顶立即合并（连续发文件/语音时不等窗口）。
const COALESCE_MAX_ITEMS: usize = 5;

/// 合并决策（纯函数，可测）：busy = 会话有未完成任务；
/// buffered = 当前缓冲条数；first_wait = 首条已等待时长。
#[derive(Debug, PartialEq, Eq)]
enum CoalesceDecision {
    /// 闲会话且无缓冲：立即入队（零延迟路径）。
    IngestNow,
    /// 攒进缓冲等静默窗口。
    Buffer,
    /// 到上限（条数/最长等待）：立即连新消息一起合并入队。
    FlushNow,
}

fn coalesce_decision(busy: bool, buffered: usize, first_wait: Duration) -> CoalesceDecision {
    if buffered > 0 && (buffered + 1 >= COALESCE_MAX_ITEMS || first_wait >= COALESCE_MAX_WAIT) {
        return CoalesceDecision::FlushNow;
    }
    if busy || buffered > 0 {
        CoalesceDecision::Buffer
    } else {
        CoalesceDecision::IngestNow
    }
}

/// 连接存活不足此时长视为「秒断」（凭据/权限问题的典型形态）。
const QUICK_DISCONNECT_THRESHOLD: Duration = Duration::from_secs(5);
/// 连续秒断达到此次数时输出诊断提示。
const QUICK_DISCONNECT_LIMIT: u32 = 3;

/// 重连退避：2^n 秒封顶 30；稳定会话（≥60s）由调用方把计数清零。
fn reconnect_delay(retries: u32) -> Duration {
    Duration::from_secs(u64::from(1_u32 << retries.min(5)).min(30))
}

impl QqBot {
    /// 「正在输入」指示去抖：同一用户 50 秒内不重复发（平台指示有效 60s）。
    async fn maybe_input_notify(&self, msg_id: &str, user_openid: &str) {
        const TYPING_DEBOUNCE: Duration = Duration::from_secs(50);
        let now = Instant::now();
        {
            let mut sent = self
                .typing_sent_at
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(last) = sent.get(user_openid)
                && now.saturating_duration_since(*last) < TYPING_DEBOUNCE
            {
                return;
            }
            sent.insert(user_openid.to_owned(), now);
        }
        let seq = self.sequencer.next(msg_id);
        if let Err(error) = self.api.send_input_notify(user_openid, msg_id, seq).await {
            eprintln!("[qq] input notify skipped: {error:#}");
        }
    }

    /// 长驻运行 gateway 事件循环，收到 shutdown 后优雅退出。
    pub async fn run(self: &Arc<Self>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
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
        let mut quick_disconnects = 0_u32;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let session_started = std::time::Instant::now();
            let result = async {
                let token = self.api.access_token().await?;
                let url = self.api.gateway_url().await?;
                gateway::run(&url, &token, &mut session, events.clone(), shutdown.clone()).await
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    let uptime = session_started.elapsed();
                    // 跑满 60s 的会话说明连接本身健康，退避计数清零；
                    // 反之秒断的连接连续出现多半是凭据/权限问题，大声提示。
                    if uptime >= Duration::from_secs(60) {
                        retries = 0;
                        quick_disconnects = 0;
                    } else if uptime < QUICK_DISCONNECT_THRESHOLD {
                        quick_disconnects += 1;
                        if quick_disconnects == QUICK_DISCONNECT_LIMIT {
                            eprintln!(
                                "[qq] 连续 {quick_disconnects} 次秒级断线：检查 AppID/Secret \
                                 与开放平台权限（intents 未授权会连上即断）"
                            );
                        }
                    }
                    retries += 1;
                    let delay = reconnect_delay(retries);
                    eprintln!(
                        "[qq] gateway disconnected after {uptime:?}: {error:#}; \
                         {delay:?} 后重连。QQ 无消息重放，断线期间发来的消息不会补发"
                    );
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
    async fn handle(self: &Arc<Self>, event: &GatewayEvent) -> Result<()> {
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(self.dispatch(event))
        })?;
        let DispatchOutcome::Reply(reply) = outcome else {
            // 消息已入队成任务：agent 要跑一会儿，先亮「正在输入」告诉用户
            // 这边活着。仅单聊（平台 input_notify 只支持 C2C）；去抖 50s；
            // 失败静默——体验增强不侵入任务链路。
            if let GatewayEvent::C2cMessage {
                msg_id,
                user_openid,
                ..
            } = event
            {
                self.maybe_input_notify(msg_id, user_openid).await;
            }
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
    async fn dispatch(self: &Arc<Self>, event: &GatewayEvent) -> Result<DispatchOutcome> {
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
        // 附件透传给插件命令（如课堂录音拦截）；gateway 里的 URL 去过协议前缀，这里补回。
        let media: Vec<crate::ports::channel::MediaRef> = event_attachments(event)
            .iter()
            .map(|a| crate::ports::channel::MediaRef {
                kind: match a.kind {
                    gateway::AttachmentKind::Image => crate::ports::channel::MediaKind::Image,
                    gateway::AttachmentKind::Voice => crate::ports::channel::MediaKind::Voice,
                    gateway::AttachmentKind::Video => crate::ports::channel::MediaKind::Video,
                    gateway::AttachmentKind::File => crate::ports::channel::MediaKind::File,
                },
                source: format!("https://{}", a.url),
                filename: a.filename.clone(),
                // CDN 下载认证快照：token 由 WS 连接时刷新，ingest 是同步热路径
                // 不做网络刷新，取缓存值即可（下载发生在收到消息的数秒内）。
                auth: self
                    .api
                    .cached_access_token()
                    .map(|token| format!("QQBot {token}")),
            })
            .collect();
        let ctx = crate::ports::commands::CommandContext {
            store: &self.store,
            bind_secret: self.bind_secret.as_deref(),
            target: &target,
            msg_id,
            author_openid,
            is_c2c,
            image_url,
            attachments: &media,
            reply_channel: "qq",
        };
        // 每条入站消息留一行痕迹：断线丢消息/附件被 QQ 静默吞掉时有据可查
        // （2026-11-04 录音丢失排障教训：此前入站零日志，无法判断消息到没到）。
        let attach_summary = media
            .iter()
            .map(|m| format!("{:?}:{}", m.kind, m.filename.as_deref().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join(",");
        eprintln!(
            "[qq.in] {} msg_id={} content_chars={} attachments=[{}]",
            if is_c2c { "c2c" } else { "group" },
            msg_id,
            content.chars().count(),
            attach_summary,
        );
        // 图片独立通道：模块消费者优先（如二维码照片签到）。都不认的图片
        // 保持原行为（image_url 已随 CommandContext 交 agent 处理）。
        if !self.image_consumers.is_empty()
            && let Some(url) = image_url
        {
            for consumer in &self.image_consumers {
                if let Some(text) = consumer.handle_image(url).await {
                    return Ok(DispatchOutcome::Reply(Reply::text(text)));
                }
            }
        }
        // 开放题路由：会话里有待答复的开放交互（pi input/editor）时，
        // 这条消息就是答案——不走命令、不入队、不参与合并排水。
        if let Some(reply) = self
            .answer_open_interaction(&target, author_openid, is_c2c, content)
            .await?
        {
            return Ok(DispatchOutcome::Reply(reply));
        }
        if let Some(command) = commands::parse_command(content) {
            let reply = if command == commands::Command::Help {
                // 帮助文本需汇总扩展命令，单独构造；按钮与 / execute 一致。
                let mut reply = commands::execute(&commands::Command::Help, &ctx, &self.pricing)?;
                reply.text = commands::help_text(&self.command_handlers);
                reply
            } else {
                commands::execute(&command, &ctx, &self.pricing)?
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
        let receipt = match self.coalesce_or_ingest(event).await? {
            // 已进缓冲：合并后的任务由 flush 任务落盘，这里不再回执。
            CoalesceOutcome::Buffered => return Ok(DispatchOutcome::Ingested),
            CoalesceOutcome::Receipt(receipt) => receipt,
        };
        // 只有发生打断才即时回复；正常入队不再发接单回执，
        // 用户直接看活动流（原生 codex 消息）即可。
        if let Some(text) = receipt.interrupt_text() {
            return Ok(DispatchOutcome::Reply(text.into()));
        }
        Ok(DispatchOutcome::Ingested)
    }

    /// 合并排水入口：闲会话立即入队；忙会话把消息攒进缓冲，
    /// 静默窗口结束后由 flush 合并成一个任务。
    async fn coalesce_or_ingest(self: &Arc<Self>, event: &GatewayEvent) -> Result<CoalesceOutcome> {
        let (msg_id, target, author, is_c2c) = match event {
            GatewayEvent::GroupMessage {
                msg_id,
                group_openid,
                author_openid,
                ..
            } => (
                msg_id.clone(),
                format!("{TARGET_GROUP_PREFIX}{group_openid}"),
                author_openid.clone(),
                false,
            ),
            GatewayEvent::C2cMessage {
                msg_id,
                user_openid,
                ..
            } => (
                msg_id.clone(),
                format!("{TARGET_C2C_PREFIX}{user_openid}"),
                user_openid.clone(),
                true,
            ),
            _ => bail!("unsupported gateway event for ingest"),
        };
        let prompt = render_message_prompt(
            match event {
                GatewayEvent::GroupMessage { content, .. } => content,
                GatewayEvent::C2cMessage { content, .. } => content,
                _ => "",
            },
            event_attachments(event),
        );
        let busy = self.conversation_busy(&target)?;
        let key = format!("{target}|{author}");
        let notify_msg_id = msg_id.clone();
        enum Action {
            Ingest,
            Buffer(u64),
            Flush,
        }
        let action = {
            let mut state = self
                .coalesce
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let buffer = state.buffers.entry(key.clone()).or_default();
            buffer.is_c2c = is_c2c;
            // 平台重推去重：同 msg_id 已在缓冲里就当已处理。
            if buffer.items.iter().any(|item| item.msg_id == msg_id) {
                return Ok(CoalesceOutcome::Buffered);
            }
            let first_wait = buffer.first_at.map(|at| at.elapsed()).unwrap_or_default();
            match coalesce_decision(busy, buffer.items.len(), first_wait) {
                CoalesceDecision::IngestNow => Action::Ingest,
                CoalesceDecision::FlushNow => {
                    buffer.items.push(CoalesceItem { msg_id, prompt });
                    Action::Flush
                }
                CoalesceDecision::Buffer => {
                    if buffer.items.is_empty() {
                        buffer.first_at = Some(Instant::now());
                    }
                    buffer.items.push(CoalesceItem { msg_id, prompt });
                    buffer.generation += 1;
                    Action::Buffer(buffer.generation)
                }
            }
        };
        match action {
            Action::Ingest => Ok(CoalesceOutcome::Receipt(self.ingest(event)?)),
            Action::Buffer(generation) => {
                if is_c2c {
                    self.maybe_input_notify(&notify_msg_id, &author).await;
                }
                let bot = self.clone();
                let key = key.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(COALESCE_DEBOUNCE).await;
                    if let Err(error) = bot.flush_coalesce(&key, generation).await {
                        eprintln!("[qq] coalesce flush failed: {error:#}");
                    }
                });
                Ok(CoalesceOutcome::Buffered)
            }
            Action::Flush => {
                let (outcome, target, last_msg_id) = self.flush_coalesce_now(&key)?;
                match outcome {
                    FlushOutcome::Receipt(receipt) => Ok(CoalesceOutcome::Receipt(receipt)),
                    FlushOutcome::Steered(task_id) => {
                        // 同步 flush 路径也要给用户插话回执。
                        self.reply(
                            &target,
                            &last_msg_id,
                            &Reply::text(format!("📨 已插话给正在运行的任务 `#{task_id}`")),
                        )
                        .await?;
                        Ok(CoalesceOutcome::Buffered)
                    }
                }
            }
        }
    }

    /// 静默窗口到点：缓冲没被别人续杯（代数一致）就合并入队。
    async fn flush_coalesce(self: &Arc<Self>, key: &str, generation: u64) -> Result<()> {
        {
            let state = self
                .coalesce
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match state.buffers.get(key) {
                // 代数变了：有更新的 flush 排班，本任务让位。
                Some(buffer) if buffer.generation != generation => return Ok(()),
                None => return Ok(()),
                _ => {}
            }
        }
        let (outcome, target, last_msg_id) = self.flush_coalesce_now(key)?;
        match outcome {
            FlushOutcome::Receipt(receipt) => {
                if let Some(text) = receipt.interrupt_text() {
                    self.reply(&target, &last_msg_id, &Reply::text(text))
                        .await?;
                }
            }
            FlushOutcome::Steered(task_id) => {
                self.reply(
                    &target,
                    &last_msg_id,
                    &Reply::text(format!("📨 已插话给正在运行的任务 `#{task_id}`")),
                )
                .await?;
            }
        }
        Ok(())
    }

    /// 取出缓冲并合并入队（或插话）；返回 (出口, target, 末条 msg_id)。
    fn flush_coalesce_now(&self, key: &str) -> Result<(FlushOutcome, String, String)> {
        let (target, items, is_c2c) = {
            let mut state = self
                .coalesce
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(buffer) = state.buffers.remove(key) else {
                bail!("coalesce buffer vanished: {key}");
            };
            let target = key
                .split_once('|')
                .map(|(target, _)| target.to_owned())
                .unwrap_or_default();
            (target, buffer.items, buffer.is_c2c)
        };
        let Some(last) = items.last() else {
            bail!("coalesce buffer empty: {key}");
        };
        let last_msg_id = last.msg_id.clone();
        let merged = items
            .into_iter()
            .map(|item| item.prompt)
            .collect::<Vec<_>>()
            .join("\n\n");
        // pi steer：会话正忙且跑的是 pi 底座——把合并后的消息插进活着的进程，
        // 不打断不重跑（OpenMausBot 的 steer-first 语义）；其余情况维持
        // 「取消旧任务 + 新任务重跑」。
        if let Some(task) = self.steerable_task(&target)? {
            let run_id = format!("run-{}-{}", task.id, task.active_attempt_id.unwrap_or(0));
            let written = self.store.put_document(
                "steer",
                &last_msg_id,
                None,
                &serde_json::json!({
                    "run_id": run_id,
                    "task_id": task.id,
                    "text": merged,
                    "status": "pending",
                    "notify": task.notify,
                    "created_at": crate::util::now(),
                }),
            )?;
            if written {
                return Ok((FlushOutcome::Steered(task.id), target, last_msg_id));
            }
            // 同 msg_id 已写过（flush 重试）：按已插话处理，不重复写。
            return Ok((FlushOutcome::Steered(task.id), target, last_msg_id));
        }
        let event = if is_c2c {
            GatewayEvent::C2cMessage {
                msg_id: last_msg_id.clone(),
                content: merged,
                user_openid: String::new(),
                image_url: None,
                attachments: Vec::new(),
            }
        } else {
            GatewayEvent::GroupMessage {
                msg_id: last_msg_id.clone(),
                content: merged,
                group_openid: String::new(),
                author_openid: String::new(),
                attachments: Vec::new(),
            }
        };
        let receipt = self.ingest(&event)?;
        Ok((FlushOutcome::Receipt(receipt), target, last_msg_id))
    }

    /// 开放题应答：会话有待答复的开放交互时，把消息内容写回交互。
    /// 单聊直接答；群聊里只认绑定用户（与 /答复 同一权限边界）。
    async fn answer_open_interaction(
        &self,
        target: &str,
        author_openid: &str,
        is_c2c: bool,
        content: &str,
    ) -> Result<Option<Reply>> {
        let now = crate::util::now();
        let mut candidates = Vec::new();
        for doc in self.store.documents("interaction", 100)? {
            let Ok(interaction) =
                serde_json::from_value::<crate::domain::records::Interaction>(doc.value)
            else {
                continue;
            };
            if is_open_question(&interaction, target, now) {
                candidates.push(interaction);
            }
        }
        let Some(interaction) = candidates.pop() else {
            return Ok(None);
        };
        if !is_c2c
            && !crate::app::identity::IdentityService::new(self.store.clone())
                .is_bound("qq", author_openid)?
        {
            return Ok(None);
        }
        crate::app::interactions::InteractionService {
            store: self.store.clone(),
        }
        .reply(&interaction.id, serde_json::json!({"value": content}))?;
        Ok(Some(Reply::text("## ✅ 已收到答复\n\n任务已继续执行。")))
    }

    /// 可插话的任务：同会话正在运行的 pi 任务（只有运行中的进程才插得进去）。
    fn steerable_task(&self, target: &str) -> Result<Option<crate::domain::Task>> {
        Ok(self
            .store
            .list(50)?
            .into_iter()
            .find(|task| is_steerable(task, target)))
    }

    /// 会话是否有未完成（排队/运行/等待重试）的任务。
    fn conversation_busy(&self, target: &str) -> Result<bool> {
        use crate::domain::TaskStatus;
        Ok(self.store.list(50)?.iter().any(|task| {
            matches!(
                task.status,
                TaskStatus::Queued | TaskStatus::Running | TaskStatus::RetryScheduled
            ) && task.notify.as_ref().map(|route| route.target.as_str()) == Some(target)
        }))
    }

    /// 命令的即时被动回复；msg_seq 经共享序号器分配，与终态/进度通知不撞车。
    /// 有按钮走 markdown + keyboard 通道（平台不支持时发送层自动降级纯文本）。
    /// 文本里的 `ru41-media://N` 占位符指向 reply.media[N]，这里上传为 markdown
    /// 素材（qqbot-media:// 引用）后内嵌；上传失败降级为独立媒体消息。
    async fn reply(&self, target: &str, msg_id: &str, reply: &Reply) -> Result<()> {
        let mut text = reply.text.clone();
        let mut inlined = vec![false; reply.media.len()];
        for (index, media) in reply.media.iter().enumerate() {
            let placeholder = format!("ru41-media://{index}");
            if !text.contains(&placeholder) {
                continue;
            }
            match self.api.upload_markdown_image(media).await {
                Ok(reference) => {
                    text = text.replace(&placeholder, &reference);
                    inlined[index] = true;
                }
                Err(err) => {
                    eprintln!("[qq.reply] markdown 素材上传失败 {}: {err:#}", media.source);
                    // 清掉坏掉的图片语法，媒体走独立消息发送。
                    text = text
                        .lines()
                        .filter(|line| !line.contains(&placeholder))
                        .collect::<Vec<_>>()
                        .join("\n");
                }
            }
        }
        let seq = self.sequencer.next(msg_id);
        if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_group_markup(group, &text, markup, Some(msg_id), seq)
                        .await?;
                }
                _ => {
                    self.api
                        .send_group_text(group, &text, Some(msg_id), seq)
                        .await?;
                }
            }
        } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
            match &reply.markup {
                Some(markup) if !markup.is_empty() => {
                    self.api
                        .send_c2c_markup(user, &text, markup, Some(msg_id), seq)
                        .await?;
                }
                _ => {
                    self.api
                        .send_c2c_text(user, &text, Some(msg_id), seq)
                        .await?;
                }
            }
        } else {
            bail!("unknown qq notify target: {target}")
        }
        // 富媒体附件（未内嵌成功的）跟在文本后逐条发；单条失败只记日志，
        // 不影响已经送达的正文。
        for (index, media) in reply.media.iter().enumerate() {
            if inlined.get(index).copied().unwrap_or(false) {
                continue;
            }
            let media_seq = self.sequencer.next(msg_id);
            let result = if let Some(group) = target.strip_prefix(TARGET_GROUP_PREFIX) {
                self.api
                    .send_group_media(group, "", media, Some(msg_id), media_seq)
                    .await
            } else if let Some(user) = target.strip_prefix(TARGET_C2C_PREFIX) {
                self.api
                    .send_c2c_media(user, "", media, Some(msg_id), media_seq)
                    .await
            } else {
                continue;
            };
            if let Err(err) = result {
                eprintln!("[qq.reply] 媒体附件发送失败 {}: {err:#}", media.source);
            }
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
        let prompt = render_message_prompt(content, attachments);
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

/// 取消息事件携带的附件列表；非消息事件返回空。
fn event_attachments(event: &GatewayEvent) -> &[gateway::MessageAttachment] {
    match event {
        GatewayEvent::GroupMessage { attachments, .. }
        | GatewayEvent::C2cMessage { attachments, .. } => attachments,
        _ => &[],
    }
}

/// 合并排水的分发结果：已入缓冲（窗口结束再合并），或已入队（回执）。
enum CoalesceOutcome {
    Buffered,
    Receipt(IngestReceipt),
}

/// flush 的出口：合并入队（可能打断旧任务），或插话进正在运行的 pi 进程。
enum FlushOutcome {
    Receipt(IngestReceipt),
    /// 已写入 steer 记录；括号里是目标任务 id。
    Steered(i64),
}

/// 把消息正文与附件渲染成 prompt：语音用平台 ASR 转写，图片/文件/视频给下载地址
/// 让 agent 自取（图片先经模块消费者认领，走到这里说明无人认领）。
/// 合并排水与 ingest 共用同一份渲染，保证合并后的 prompt 形态一致。
fn render_message_prompt(content: &str, attachments: &[gateway::MessageAttachment]) -> String {
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
            gateway::AttachmentKind::Video => {
                let name = attachment.filename.as_deref().unwrap_or("未命名视频");
                prompt.push_str(&format!(
                    "[视频] {name}\n下载地址（有时效，请尽快下载）：https://{}\n",
                    attachment.url
                ));
            }
            gateway::AttachmentKind::Image => {
                // 无消费者认领的图片给下载地址进 prompt：QQ 不能图文混发，
                // 用户「先发图再发指令」的两条消息靠会话续跑/插话汇聚到同一会话，
                // agent 拿着地址自行下载处理（如存入知识库）。
                prompt.push_str(&format!(
                    "[图片] 下载地址（有时效，请尽快下载）：https://{}\n",
                    attachment.url
                ));
            }
        }
    }
    if !content.is_empty() {
        if !prompt.is_empty() {
            prompt.push('\n');
        }
        prompt.push_str(content);
    }
    prompt
}

/// 开放交互判定（纯函数，可测）：pending、未过期、pi 开放题（input/editor）、
/// 属于该会话。开放题没有选项，用户在 QQ 的下一条回复就是答案。
fn is_open_question(
    interaction: &crate::domain::records::Interaction,
    target: &str,
    now: i64,
) -> bool {
    interaction.status == "pending"
        && interaction.expires_at > now
        && matches!(interaction.method.as_str(), "pi/input" | "pi/editor")
        && interaction.options.is_empty()
        && interaction
            .notify
            .as_ref()
            .is_some_and(|route| route.target == target)
}

/// 可插话判定（纯函数，可测）：运行中的 pi 任务且属于该会话。
/// 排队中的不行（进程还没起）、codex 不行（适配器没接 steer）。
fn is_steerable(task: &crate::domain::Task, target: &str) -> bool {
    task.status == crate::domain::TaskStatus::Running
        && task.backend_id.starts_with("pi")
        && task.notify.as_ref().map(|route| route.target.as_str()) == Some(target)
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
        (self.interrupted > 0)
            .then(|| format!("⏹ 已打断上一条未完成的任务（{} 个）", self.interrupted))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        COALESCE_MAX_WAIT, CoalesceDecision, QUICK_DISCONNECT_THRESHOLD, coalesce_decision,
        reconnect_delay,
    };
    use std::time::Duration;

    pub(crate) fn task_stub() -> crate::domain::Task {
        crate::domain::Task {
            id: 1,
            request_key: "k".into(),
            backend_id: "pi".into(),
            contract: crate::domain::RunContract::AnalysisV1,
            prompt: "p".into(),
            status: crate::domain::TaskStatus::Queued,
            created_at: String::new(),
            updated_at: String::new(),
            attempt_count: 0,
            max_attempts: 3,
            active_attempt_id: Some(1),
            lease_until: None,
            available_at: 0,
            result: None,
            last_error: None,
            notify: None,
            notified_at: None,
        }
    }

    #[test]
    fn coalesce_idle_ingests_immediately() {
        // 闲会话零延迟：不管有没有等待都直接入队。
        assert_eq!(
            coalesce_decision(false, 0, Duration::ZERO),
            CoalesceDecision::IngestNow
        );
    }

    #[test]
    fn coalesce_busy_buffers_and_caps() {
        // 忙会话进缓冲；缓冲到顶或超过最长等待立即合并。
        assert_eq!(
            coalesce_decision(true, 0, Duration::ZERO),
            CoalesceDecision::Buffer
        );
        assert_eq!(
            coalesce_decision(true, 3, Duration::from_secs(5)),
            CoalesceDecision::Buffer
        );
        assert_eq!(
            coalesce_decision(true, 4, Duration::from_secs(5)),
            CoalesceDecision::FlushNow
        );
        assert_eq!(
            coalesce_decision(true, 1, COALESCE_MAX_WAIT),
            CoalesceDecision::FlushNow
        );
        // 已有缓冲时新消息一律进缓冲（保持顺序），即使任务刚好跑完。
        assert_eq!(
            coalesce_decision(false, 2, Duration::from_secs(3)),
            CoalesceDecision::Buffer
        );
    }

    #[test]
    fn steerable_only_for_running_pi_in_same_conversation() {
        use crate::domain::{NotifyRoute, TaskStatus};
        let mut task = super::tests::task_stub();
        task.status = TaskStatus::Running;
        task.backend_id = "pi".into();
        task.notify = Some(NotifyRoute {
            channel: "qq".into(),
            target: "user:U1".into(),
            msg_id: None,
        });
        assert!(super::is_steerable(&task, "user:U1"));
        // 排队中的插不进去。
        task.status = TaskStatus::Queued;
        assert!(!super::is_steerable(&task, "user:U1"));
        // codex 底座没接 steer。
        task.status = TaskStatus::Running;
        task.backend_id = "codex".into();
        assert!(!super::is_steerable(&task, "user:U1"));
        // 别的会话不算。
        task.backend_id = "pi".into();
        assert!(!super::is_steerable(&task, "user:U2"));
    }

    #[test]
    fn open_question_matches_pending_input_in_same_conversation() {
        use crate::domain::records::Interaction;
        let base = Interaction {
            id: "r-1".into(),
            run_id: "r".into(),
            request_id: serde_json::json!(1),
            method: "pi/input".into(),
            detail: serde_json::json!({"title": "文件名？"}),
            options: vec![],
            expires_at: 200,
            status: "pending".into(),
            response: None,
            notify: Some(crate::domain::NotifyRoute {
                channel: "qq".into(),
                target: "user:U1".into(),
                msg_id: None,
            }),
            dismiss_response: None,
            notified: false,
        };
        assert!(super::is_open_question(&base, "user:U1", 100));
        // 过期、已答复、别的会话、选项题都不算。
        assert!(!super::is_open_question(&base, "user:U1", 300));
        let mut answered = base.clone();
        answered.status = "answered".into();
        assert!(!super::is_open_question(&answered, "user:U1", 100));
        assert!(!super::is_open_question(&base, "user:U2", 100));
        let mut select = base.clone();
        select.method = "pi/select".into();
        select.options = vec![serde_json::json!({"label": "A"})];
        assert!(!super::is_open_question(&select, "user:U1", 100));
    }

    #[test]
    fn reconnect_backoff_grows_and_caps() {
        assert_eq!(reconnect_delay(1), Duration::from_secs(2));
        assert_eq!(reconnect_delay(2), Duration::from_secs(4));
        assert_eq!(reconnect_delay(5), Duration::from_secs(30));
        assert_eq!(reconnect_delay(50), Duration::from_secs(30));
    }

    #[test]
    fn quick_disconnect_threshold_is_tight() {
        assert_eq!(QUICK_DISCONNECT_THRESHOLD, Duration::from_secs(5));
    }
}
