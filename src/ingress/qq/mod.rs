pub use crate::adapters::messaging::qq::{api, gateway};

use crate::{domain::NotifyRoute, ports::channel::NotifyChannel, ports::storage::TaskStore};
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

/// QQ 网关：收消息幂等入队任务；终态回复由 runtime::dispatcher 的 dispatcher 统一负责。
pub struct QqBot {
    pub api: Arc<QqApiClient>,
    pub store: Arc<dyn TaskStore>,
    pub backend: String,
}

impl QqBot {
    /// 长驻运行 gateway 事件循环，收到 shutdown 后优雅退出。
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let (events, mut receiver) = mpsc::channel::<GatewayEvent>(256);
        let gateway = self.gateway_loop(events, shutdown.clone());
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
