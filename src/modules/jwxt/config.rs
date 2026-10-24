use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

use super::service::{JwxtConfig, SectionClock};

/// 教务课表监控配置；jwxt poll/run/login 使用。协议见 docs/jwxt-recon.md。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JwxtSection {
    /// 统一身份认证账号（学号，建议环境变量 RU41_JWXT_USERNAME 注入，勿提交）。
    pub username: Option<String>,
    /// 统一身份认证密码（建议环境变量 RU41_JWXT_PASSWORD 注入，勿提交）。
    pub password: Option<String>,
    /// CAS 基址（联奕 lyuapServer）。
    pub cas_base: Option<String>,
    /// CAS 前端 RSA 公钥 modulus（hex，校方部署配置，通常无需修改）。
    pub cas_modulus: Option<String>,
    /// CAS 前端 RSA 公钥 exponent（hex，默认 010001）。
    pub cas_exponent: Option<String>,
    /// 教务基址。
    pub jwxt_base: Option<String>,
    /// 会话持久化路径（TGT + cookie）；默认 .ru41/jwxt/session.json。
    pub session_path: Option<String>,
    /// 学期第一周周一（如 2026-09-07）；周次推算与明日课表依赖它。
    pub semester_start: Option<String>,
    /// 学年起始年（如 2026）；默认按日期自动推断。
    pub xnm: Option<String>,
    /// 学期码（3=一/12=二/16=三）；默认按日期自动推断。
    pub xqm: Option<String>,
    /// jwxt run 的轮询间隔（秒），默认 3600。
    pub interval_secs: Option<u64>,
    /// 是否推送明日课表（每日一次）；默认 true。
    pub daily_remind: Option<bool>,
    /// 明日课表推送的本地时间下限（小时），默认 21。
    pub remind_hour: Option<u8>,
    /// 是否把课表同步进日程系统（event records，课前 20 分钟提醒、`/日程` 可见）。
    /// 开启需同时配置 section_times。默认 false。
    pub sync_schedule: Option<bool>,
    /// 节次上课时刻表（第 1..=16 节的 "HH:MM"，索引=节次-1）；教务接口不暴露
    /// 作息时间，需按学校实际填写。同步日程依赖它。
    pub section_times: Option<Vec<String>>,
    /// 单节课时长（分钟），默认 45。
    pub class_duration_mins: Option<u64>,
    /// 检测到课表变更/登录失效是否经默认通知出口提醒；默认 true。
    pub notify: Option<bool>,
}

impl JwxtSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 3_600;
    /// 明日课表推送时刻默认值（本地小时）。
    pub const DEFAULT_REMIND_HOUR: u8 = 21;
    /// 单节课时长默认值（分钟）。
    pub const DEFAULT_CLASS_DURATION_MINS: u64 = 45;
    /// CAS 基址默认值。
    pub const DEFAULT_CAS_BASE: &str = "https://cas.gzus.edu.cn/lyuapServer";
    /// 教务基址默认值。
    pub const DEFAULT_JWXT_BASE: &str = "https://jwxt.gzus.edu.cn";
    /// CAS 前端 RSA modulus 默认值（该校部署配置；改版时经本地配置覆盖）。
    pub const DEFAULT_CAS_MODULUS: &str = "00b5eeb166e069920e80bebd1fea4829d3d1f3216f2aabe79b6c47a3c18dcee5fd22c2e7ac519cab59198ece036dcf289ea8201e2a0b9ded307f8fb704136eaeb670286f5ad44e691005ba9ea5af04ada5367cd724b5a26fdb5120cc95b6431604bd219c6b7d83a6f8f24b43918ea988a76f93c333aa5a20991493d4eb1117e7b1";
    pub const DEFAULT_CAS_EXPONENT: &str = "010001";
    /// 会话文件默认路径。
    pub const DEFAULT_SESSION_PATH: &str = ".ru41/jwxt/session.json";

    /// 由 [sources.jwxt] 解析出运行时配置；默认值与凭证兜底只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<JwxtConfig> {
        let username = std::env::var("RU41_JWXT_USERNAME")
            .ok()
            .or_else(|| self.username.clone())
            .filter(|v| !v.trim().is_empty())
            .context("jwxt username required: RU41_JWXT_USERNAME or sources.jwxt.username")?;
        let password = std::env::var("RU41_JWXT_PASSWORD")
            .ok()
            .or_else(|| self.password.clone())
            .filter(|v| !v.trim().is_empty())
            .context("jwxt password required: RU41_JWXT_PASSWORD or sources.jwxt.password")?;
        let semester_start = match &self.semester_start {
            Some(raw) => Some(
                NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d").with_context(|| {
                    format!("sources.jwxt.semester_start 格式应为 YYYY-MM-DD: {raw}")
                })?,
            ),
            None => None,
        };
        Ok(JwxtConfig {
            username,
            password,
            cas_base: self
                .cas_base
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_CAS_BASE.into()),
            cas_modulus: self
                .cas_modulus
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_CAS_MODULUS.into()),
            cas_exponent: self
                .cas_exponent
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_CAS_EXPONENT.into()),
            jwxt_base: self
                .jwxt_base
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_JWXT_BASE.into()),
            session_path: self
                .session_path
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_SESSION_PATH.into())
                .into(),
            semester_start,
            xnm: self.xnm.clone(),
            xqm: self.xqm.clone(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            daily_remind: self.daily_remind.unwrap_or(true),
            remind_hour: self.remind_hour.unwrap_or(Self::DEFAULT_REMIND_HOUR),
            sync_schedule: self.sync_schedule.unwrap_or(false),
            clock: self.section_clock()?,
            notify_target,
        })
    }

    /// 节次时刻表（不需要凭证即可解析，QQ 命令装配与 `resolved` 共用）。
    /// 未配置 section_times 返回 None，消息回退到节次文本。
    pub fn section_clock(&self) -> Result<Option<SectionClock>> {
        let Some(raw_list) = &self.section_times else {
            return Ok(None);
        };
        raw_list
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                chrono::NaiveTime::parse_from_str(raw.trim(), "%H:%M").with_context(|| {
                    format!(
                        "sources.jwxt.section_times[{}] 格式应为 HH:MM: {raw}",
                        i + 1
                    )
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(|times| {
                Some(SectionClock {
                    times,
                    duration_mins: self
                        .class_duration_mins
                        .unwrap_or(Self::DEFAULT_CLASS_DURATION_MINS),
                })
            })
    }

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        (self.username.is_some() || std::env::var("RU41_JWXT_USERNAME").is_ok())
            && (self.password.is_some() || std::env::var("RU41_JWXT_PASSWORD").is_ok())
    }
}
