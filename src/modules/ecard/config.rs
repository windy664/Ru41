use anyhow::{Context, Result};
use serde::Deserialize;

use super::service::EcardConfig;
use super::source::EcardRoom;

/// 校园卡水电监控配置；ecard poll/run 使用。协议见 docs/ecard-waterfee.md。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EcardSection {
    /// ecard 平台 API 基址（如 https://ecarduser.<school>.edu.cn），学校域名只存本地。
    pub base_url: Option<String>,
    /// ecard 平台 token（建议用环境变量 RU41_ECARD_TOKEN 注入，勿提交）。
    pub token: Option<String>,
    /// 房间定位参数（getBalance 四项）。
    pub impl_type: Option<String>,
    pub school_area_no: Option<String>,
    pub building_no: Option<String>,
    pub room_num: Option<String>,
    /// 电量低余额阈值（度），默认 10。
    pub power_threshold: Option<f64>,
    /// 水量低余额阈值（吨），默认 1。
    pub water_threshold: Option<f64>,
    /// ecard run 的轮询间隔（秒），默认 86400（每日）。
    pub interval_secs: Option<u64>,
    /// 低余额时是否经默认通知出口提醒；默认 true。
    pub notify: Option<bool>,
    /// 每日推送余额日报（健康读数也发一条）；默认 false。
    pub daily_report: Option<bool>,
}

impl EcardSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 86_400;
    /// 低余额提醒阈值默认值。
    pub const DEFAULT_POWER_THRESHOLD: f64 = 10.0;
    pub const DEFAULT_WATER_THRESHOLD: f64 = 1.0;

    /// 由 [sources.ecard] 解析出运行时配置；默认值与 RU41_ECARD_TOKEN 兜底只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<EcardConfig> {
        let token = std::env::var("RU41_ECARD_TOKEN")
            .ok()
            .or_else(|| self.token.clone())
            .filter(|t| !t.trim().is_empty())
            .context("ecard token required: RU41_ECARD_TOKEN or sources.ecard.token")?;
        let room = EcardRoom {
            impl_type: self
                .impl_type
                .clone()
                .context("sources.ecard.impl_type required")?,
            school_area_no: self
                .school_area_no
                .clone()
                .context("sources.ecard.school_area_no required")?,
            building_no: self
                .building_no
                .clone()
                .context("sources.ecard.building_no required")?,
            room_num: self
                .room_num
                .clone()
                .context("sources.ecard.room_num required")?,
        };
        Ok(EcardConfig {
            room,
            token,
            power_threshold: self
                .power_threshold
                .unwrap_or(Self::DEFAULT_POWER_THRESHOLD),
            water_threshold: self
                .water_threshold
                .unwrap_or(Self::DEFAULT_WATER_THRESHOLD),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify_target,
            daily_report: self.daily_report.unwrap_or(false),
        })
    }

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        self.base_url.is_some()
            && self.impl_type.is_some()
            && self.school_area_no.is_some()
            && self.building_no.is_some()
            && self.room_num.is_some()
            && (self.token.is_some() || std::env::var("RU41_ECARD_TOKEN").is_ok())
    }
}
