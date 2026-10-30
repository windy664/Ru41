use anyhow::Result;
use ru41::{ports::channel::NotifyChannel, ports::storage::TaskStore};
use serde::Serialize;
use std::sync::Arc;

use crate::source::{CapacitySpec, CompshareClient, StockState};

/// 恒源云库存监控运行时配置（对应 [sources.compshare]）。
#[derive(Debug, Clone)]
pub struct CompshareConfig {
    pub base_url: String,
    pub project_id: String,
    pub image_id: String,
    pub region: String,
    pub zone: String,
    pub gpu_type: String,
    pub disk_size_gb: u64,
    pub interval_secs: u64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
    /// 控制台完整 Cookie；只用于 API 请求，不写库、不进日志。
    pub cookie: String,
}

impl CompshareConfig {
    pub fn page_url(&self) -> String {
        format!(
            "https://console.compshare.cn/light-gpu/resources/create?ImageId={}",
            self.image_id
        )
    }

    pub fn record_id(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.image_id, self.region, self.zone, self.gpu_type
        )
    }
}

impl Default for CompshareConfig {
    /// 语义默认值与 CompshareSection::resolved 对齐；测试构造只需写关心字段。
    fn default() -> Self {
        Self {
            base_url: "http://localhost".into(),
            project_id: "project".into(),
            image_id: super::config::CompshareSection::DEFAULT_IMAGE_ID.into(),
            region: super::config::CompshareSection::DEFAULT_REGION.into(),
            zone: super::config::CompshareSection::DEFAULT_ZONE.into(),
            gpu_type: super::config::CompshareSection::DEFAULT_GPU_TYPE.into(),
            disk_size_gb: super::config::CompshareSection::DEFAULT_DISK_SIZE_GB,
            interval_secs: super::config::CompshareSection::DEFAULT_INTERVAL_SECS,
            notify_target: String::new(),
            cookie: String::new(),
        }
    }
}

/// 一次轮询的报告。
#[derive(Debug, Serialize)]
pub struct CompshareReport {
    pub state: &'static str,
    pub available_specs: usize,
    pub specs: Vec<CapacitySpec>,
    pub notified: bool,
    pub auth_expired: bool,
    pub auth_alerted: bool,
}

/// 库存监控：调用创建页容量接口 → 严格按 ResourceEnough 判定 →
/// 首轮有货或无货→有货时经注入通知渠道提醒；持续状态不重复打扰。
#[derive(Clone)]
pub struct CompshareTracker {
    store: Arc<dyn TaskStore>,
    client: CompshareClient,
    notifier: Option<Arc<dyn NotifyChannel>>,
    config: CompshareConfig,
}

impl CompshareTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: CompshareClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: CompshareConfig,
    ) -> Self {
        Self {
            store,
            client,
            notifier,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<CompshareReport> {
        let snapshot = self.client.check_capacity(&self.config).await?;
        let now = ru41::util::now();
        if snapshot.state == StockState::AuthExpired {
            return self.handle_expired(now).await;
        }
        let available: Vec<_> = snapshot
            .specs
            .iter()
            .filter(|spec| spec.resource_enough)
            .cloned()
            .collect();
        let state = if available.is_empty() {
            "unavailable"
        } else {
            "available"
        };
        let record_id = self.config.record_id();
        let existing = self.store.document("compshare_state", &record_id)?;
        let previous_state = existing
            .as_ref()
            .and_then(|doc| doc.value["state"].as_str())
            .unwrap_or("unknown");
        let previous_notified_at = existing
            .as_ref()
            .and_then(|doc| doc.value["notified_at"].as_i64())
            .unwrap_or(0);
        let mut notified = false;
        // 首轮有货也报；通知失败直接返回错误，下轮按旧状态重试，不能静默丢提醒。
        if state == "available"
            && previous_state != "available"
            && let Some(notifier) = &self.notifier
        {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &available_message(&self.config, &available),
                )
                .await?;
            notified = true;
        }
        let written = self.store.put_document(
            "compshare_state",
            &record_id,
            existing.map(|doc| doc.revision),
            &serde_json::json!({
                "state": state,
                "specs": snapshot.specs,
                "checked_at": now,
                "notified_at": if notified { now } else { previous_notified_at },
            }),
        )?;
        anyhow::ensure!(written, "compshare_state 被并发修改，库存状态未保存");
        Ok(CompshareReport {
            state,
            available_specs: available.len(),
            specs: available,
            notified,
            auth_expired: false,
            auth_alerted: false,
        })
    }

    async fn handle_expired(&self, now: i64) -> Result<CompshareReport> {
        let today = ru41::util::today_utc(now);
        let mut auth_alerted = false;
        if self
            .store
            .document("compshare_auth_alert", &today)?
            .is_none()
            && let Some(notifier) = &self.notifier
        {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &format!(
                        "## ⚠️ 恒源云登录态失效\n\n暂时无法查询 {} 的 {} 库存。请重新登录后更新 RU41_COMPSHARE_COOKIE。",
                        self.config.zone, self.config.gpu_type
                    ),
                )
                .await?;
            self.store.put_document(
                "compshare_auth_alert",
                &today,
                None,
                &serde_json::json!({ "ts": now }),
            )?;
            auth_alerted = true;
        }
        Ok(CompshareReport {
            state: "auth_expired",
            available_specs: 0,
            specs: Vec::new(),
            notified: false,
            auth_expired: true,
            auth_alerted,
        })
    }
}

fn available_message(config: &CompshareConfig, specs: &[CapacitySpec]) -> String {
    format!(
        "## 🎮 恒源云 GPU 有货\n\n- 机房：{}\n- 显卡：{}\n- 可创建规格：{}\n\n创建页：{}",
        config.zone,
        config.gpu_type,
        specs
            .iter()
            .map(|spec| format!("{}×GPU / {}C / {}GB", spec.gpu, spec.cpu, spec.mem))
            .collect::<Vec<_>>()
            .join("，"),
        config.page_url(),
    )
}

impl ru41::ingress::schedule::PollJob for CompshareTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self.clone();
        ru41::plugins::bridge(async move { Ok(serde_json::to_value(owned.poll_once().await?)?) })
    }
}
