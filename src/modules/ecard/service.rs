use crate::{ports::channel::NotifyChannel, ports::storage::TaskStore};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

use super::source::{EcardClient, EcardRoom};

/// 水电监控配置（对应配置文件的 [sources.ecard]）。
#[derive(Debug, Clone)]
pub struct EcardConfig {
    pub room: EcardRoom,
    pub token: String,
    /// 电量低余额阈值（度）。
    pub power_threshold: f64,
    /// 水量低余额阈值（吨）。
    pub water_threshold: f64,
    pub interval_secs: u64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析，如 feishu 的 default 或 qq 的 user:{openid}）。
    pub notify_target: String,
}

/// 一次轮询的报告。
#[derive(Debug, Serialize)]
pub struct EcardReport {
    pub power: Option<f64>,
    pub water: Option<f64>,
    pub power_label: Option<String>,
    pub water_label: Option<String>,
    pub room_label: String,
    pub alert_sent: bool,
    pub alert_reason: Option<String>,
}

/// 水电监控：查询余额 → 读数落 records 形成历史 → 阈值判断（纯 Rust，不经 Agent）
/// → 低余额经注入的通知渠道提醒（同一天最多一条）。
pub struct EcardTracker {
    store: Arc<dyn TaskStore>,
    client: EcardClient,
    notifier: Option<Arc<dyn NotifyChannel>>,
    config: EcardConfig,
}

impl EcardTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: EcardClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: EcardConfig,
    ) -> Self {
        Self {
            store,
            client,
            notifier,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<EcardReport> {
        let balance = self
            .client
            .get_balance(&self.config.room, &self.config.token)
            .await?;
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        let power = balance.power_value();
        let water = balance.water_value();
        let room_label = format!(
            "{} {}",
            balance.building.as_deref().unwrap_or(""),
            balance.room.as_deref().unwrap_or("")
        )
        .trim()
        .to_owned();
        // 读数落库：同日覆盖（保留最新），跨日形成历史；余额回升即为充值。
        self.store.put_document(
            "ecard_reading",
            &today,
            None,
            &serde_json::json!({
                "power": power,
                "water": water,
                "room": room_label,
                "ts": now,
            }),
        )?;

        let mut low = Vec::new();
        if let Some(p) = power
            && p <= self.config.power_threshold
        {
            low.push(format!(
                "电 {:.2}度（阈值 {:.0}）",
                p, self.config.power_threshold
            ));
        }
        if let Some(w) = water
            && w <= self.config.water_threshold
        {
            low.push(format!(
                "水 {:.2}吨（阈值 {:.0}）",
                w, self.config.water_threshold
            ));
        }
        let mut alert_sent = false;
        let mut alert_reason = None;
        if !low.is_empty() {
            alert_reason = Some(low.join("，"));
            // 同日已提醒过就不重复打扰。
            let alerted = self.store.document("ecard_alert", &today)?.is_some();
            if !alerted && let Some(notifier) = &self.notifier {
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!(
                            "【水电提醒】{room_label}：{}。查询时间 {today}。",
                            alert_reason.as_deref().unwrap_or("")
                        ),
                    )
                    .await?;
                self.store.put_document(
                    "ecard_alert",
                    &today,
                    None,
                    &serde_json::json!({ "ts": now, "power": power, "water": water }),
                )?;
                alert_sent = true;
            }
        }
        Ok(EcardReport {
            power,
            water,
            power_label: balance.power_label,
            water_label: balance.water_label,
            room_label,
            alert_sent,
            alert_reason,
        })
    }
}

impl EcardTracker {
    /// 轮询间隔（run 定时入口使用）。
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }
}
