use anyhow::Result;
use ru41::{ports::channel::NotifyChannel, ports::storage::TaskStore};
use serde::Serialize;
use std::sync::Arc;

use crate::source::{EcardClient, EcardRoom};

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
    /// 每日推送余额日报（健康读数也发一条；低余额仍走提醒）。
    pub daily_report: bool,
}

impl Default for EcardConfig {
    /// 语义默认值与 EcardSection::resolved 对齐；测试构造只需写关心的字段。
    fn default() -> Self {
        Self {
            room: crate::source::EcardRoom::default(),
            token: String::new(),
            power_threshold: 10.0,
            water_threshold: 1.0,
            interval_secs: 86_400,
            notify_target: String::new(),
            daily_report: false,
        }
    }
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
    pub report_sent: bool,
}

/// 水电监控：查询余额 → 读数落 records 形成历史 → 阈值判断（纯 Rust，不经 Agent）
/// → 低余额经注入的通知渠道提醒（同一天最多一条）。
#[derive(Clone)]
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
        let now = ru41::util::now();
        let today = ru41::util::today_utc(now);
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
        // 注意 put_document(None) 是 insert-or-ignore，覆盖必须带现有 revision。
        let existing = self.store.document("ecard_reading", &today)?;
        let written = self.store.put_document(
            "ecard_reading",
            &today,
            existing.map(|d| d.revision),
            &serde_json::json!({
                "power": power,
                "water": water,
                "room": room_label,
                "ts": now,
            }),
        )?;
        anyhow::ensure!(written, "ecard_reading 被并发修改，读数未保存");

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
                let items = low
                    .iter()
                    .map(|item| format!("- {item}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!(
                            "## ⚡ 水电提醒\n\n- 房间：{room_label}\n{items}\n- 查询时间：{today}"
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
        // 每日日报：daily_report 开启时，每天第一条健康读数也推送余额快照；
        // ecard_report 同日去重，低余额仍单独走 ecard_alert，不重复打扰。
        let mut report_sent = false;
        if low.is_empty()
            && self.config.daily_report
            && let Some(notifier) = &self.notifier
            && self.store.document("ecard_report", &today)?.is_none()
        {
            notifier
                .send(
                    &self.config.notify_target,
                    None,
                    &format!(
                        "## ⚡ 水电日报\n\n- 房间：{room_label}\n- 电 {} / 水 {}\n- 日期：{today}\n\n> 低于阈值（电 {:.0} 度 / 水 {:.0} 吨）会单独提醒",
                        power.map_or("--".into(), |p| format!("{p:.2} 度")),
                        water.map_or("--".into(), |w| format!("{w:.2} 吨")),
                        self.config.power_threshold,
                        self.config.water_threshold,
                    ),
                )
                .await?;
            self.store.put_document(
                "ecard_report",
                &today,
                None,
                &serde_json::json!({ "ts": now, "power": power, "water": water }),
            )?;
            report_sent = true;
        }
        Ok(EcardReport {
            power,
            water,
            power_label: balance.power_label,
            water_label: balance.water_label,
            room_label,
            alert_sent,
            alert_reason,
            report_sent,
        })
    }
}

impl ru41::ingress::schedule::PollJob for EcardTracker {
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
