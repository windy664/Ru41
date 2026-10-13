pub mod api;
pub mod gateway;

use crate::{app::store::TaskStore, domain::NotifyRoute};
use anyhow::{Result, bail};
use api::QqApiClient;
use gateway::{GatewayEvent, GatewaySession};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};

/// notify_target 的前缀：区分群会话与单聊。
pub const TARGET_GROUP_PREFIX: &str = "group:";
pub const TARGET_C2C_PREFIX: &str = "user:";

/// QQ 渠道的消息请求键，保证同一 msg_id 只入队一次。
pub fn msg_request_key(msg_id: &str) -> String {
    format!("qq:msg:{msg_id}")
}

/// QQ 网关：收消息入队任务，任务到终态后被动回复原会话。
pub struct QqBot {
    pub api: Arc<QqApiClient>,
    pub store: Arc<dyn TaskStore>,
    pub backend: String,
}

impl QqBot {
    /// 长驻运行：gateway 事件循环 + 通知 dispatcher，收到 shutdown 后优雅退出。
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let (events, mut receiver) = mpsc::channel::<GatewayEvent>(256);
        let gateway = self.gateway_loop(events, shutdown.clone());
        let dispatcher = self.dispatcher_loop(shutdown.clone());
        let ingest = async {
            while let Some(event) = receiver.recv().await {
                if let Err(error) = self.ingest(&event) {
                    eprintln!("[qq] enqueue failed: {error:#}");
                }
            }
            Ok::<(), anyhow::Error>(())
        };
        // 任一分支结束即整体退出，其余 future 随 select 取消。
        tokio::select! {
            biased;
            _ = shutdown.changed() => {}
            result = gateway => result?,
            result = dispatcher => result?,
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

    /// 轮询已到终态但未通知的任务，发被动回复并标记。
    async fn dispatcher_loop(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut seq_by_msg: HashMap<String, i64> = HashMap::new();
        let mut poll = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                _ = poll.tick() => {}
            }
            let tasks = self.store.list_unnotified(20)?;
            for task in tasks {
                let Some(notify) = &task.notify else { continue };
                if notify.channel != "qq" {
                    continue;
                }
                let content = match &task.result {
                    Some(result) => result.summary.clone(),
                    None => format!(
                        "任务失败：{}",
                        task.last_error.as_deref().unwrap_or("未知错误")
                    ),
                };
                let seq = seq_by_msg
                    .get(notify.msg_id.as_deref().unwrap_or_default())
                    .copied()
                    .unwrap_or(0)
                    + 1;
                let send = if let Some(group) = notify.target.strip_prefix(TARGET_GROUP_PREFIX) {
                    self.api
                        .send_group_text(group, &content, notify.msg_id.as_deref(), seq)
                        .await
                } else if let Some(user) = notify.target.strip_prefix(TARGET_C2C_PREFIX) {
                    self.api
                        .send_c2c_text(user, &content, notify.msg_id.as_deref(), seq)
                        .await
                } else {
                    bail!("unknown qq notify target: {}", notify.target)
                };
                match send {
                    Ok(_) => {
                        seq_by_msg.insert(notify.msg_id.clone().unwrap_or_default(), seq);
                        self.store.mark_notified(task.id)?;
                    }
                    Err(error) => eprintln!("[qq] notify task {} failed: {error:#}", task.id),
                }
            }
        }
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
        self.store.enqueue_with_notify(
            &msg_request_key(msg_id),
            content,
            &self.backend,
            crate::domain::RunContract::AnalysisV1,
            &NotifyRoute {
                channel: "qq".into(),
                target,
                msg_id: Some(msg_id.clone()),
            },
        )?;
        Ok(())
    }
}
