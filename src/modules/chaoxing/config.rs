use anyhow::{Context, Result};
use serde::Deserialize;

/// 学习通签到监控运行时配置（由 ChaoxingSection::resolved 解析产出）。
#[derive(Debug, Clone)]
pub struct ChaoxingConfig {
    pub phone: String,
    pub password: String,
    /// 会话 cookie 持久化路径。
    pub cookie_path: std::path::PathBuf,
    pub interval_secs: u64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
    /// 自动签到：poll 发现待签活动时当场尝试（全类型；二维码 enc 为伪造值，
    /// 平台不校验真实性，被拒时打标记并回执）。
    pub auto_sign: bool,
    /// 默认位置（坐标 + 地址文本）：位置/二维码签到的坐标缺省来源。
    pub default_location: Option<(f64, f64, String)>,
    /// 拍照签到默认照片路径；未配置且命令行未给 --photo 时用内置兜底照片。
    pub photo_path: Option<std::path::PathBuf>,
    /// 作业轮询间隔（秒）。
    pub work_poll_secs: u64,
    /// 截止前 N 天开始临期提醒。
    pub work_due_days: i64,
    /// 未交作业自动进待办、平台已交自动回勾。
    pub work_todo_sync: bool,
    /// 作业清单同步进 Obsidian 课程目录 + 跨平台作业总表。
    pub work_vault_sync: bool,
    /// Vault 内课程所在学期目录；None 表示不落 vault。
    pub vault_course_dir: Option<String>,
    /// 学习通课程名 → Vault 课程目录名映射。
    pub vault_courses: std::collections::BTreeMap<String, String>,
    /// 作业任务目录策略；substantive 下只给未交作业建目录。
    pub work_vault_task_dirs: crate::ports::vault::VaultTaskDirs,
    /// enc 样本库路径（科研线：QR 窗口探测 + 照片解码样本的 JSONL 落盘）；
    /// None 关闭采集。缺省 /data/chaoxing/enc-samples.jsonl。
    pub enc_sample_path: Option<std::path::PathBuf>,
}

/// 学习通签到监控配置；chaoxing poll/run/sign 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChaoxingSection {
    /// 学习通手机号（建议用环境变量 RU41_XXT_PHONE 注入，勿提交）。
    pub phone: Option<String>,
    /// 学习通密码（建议用环境变量 RU41_XXT_PASSWORD 注入，勿提交）。
    pub password: Option<String>,
    /// 会话 cookie 持久化路径；默认 .ru41/chaoxing/cookies.json。
    pub cookie_path: Option<String>,
    /// chaoxing run 的轮询间隔（秒），默认 60（签到窗口常为 1-2 分钟，
    /// 太疏会漏签；代价是每轮 3 个只读请求，介意风控可调大）。
    pub interval_secs: Option<u64>,
    /// 检测到待签活动是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// 自动签到：poll 发现待签活动时当场尝试签到（除二维码外全类型）。
    /// 平台拒绝后不自动重试；网络错误下轮自然重试。默认 false。
    pub auto_sign: Option<bool>,
    /// 默认纬度（位置/二维码签到的坐标缺省来源，如教学楼）。
    pub default_lat: Option<f64>,
    /// 默认经度。
    pub default_lon: Option<f64>,
    /// 默认地址文本（位置签到的 address 参数，教师端可见）。
    pub default_addr: Option<String>,
    /// 拍照签到默认照片路径；缺省用内置兜底照片。
    pub photo_path: Option<String>,
    /// 作业轮询间隔（秒），默认 1800。作业列表请求比签到重，务必低频。
    pub work_poll_secs: Option<u64>,
    /// 截止前 N 天开始临期提醒，默认 3。
    pub work_due_days: Option<i64>,
    /// 未交作业自动进待办（平台已交时自动回勾），默认 true。
    pub work_todo_sync: Option<bool>,
    /// 作业清单同步进 Obsidian 课程目录 + 跨平台作业总表，默认 true。
    pub work_vault_sync: Option<bool>,
    /// Vault 内课程所在学期目录（与 [sources.zhihuishu] 指向同一目录实现聚合）。
    pub vault_course_dir: Option<String>,
    /// 学习通课程名 → Vault 课程目录名映射。
    pub vault_courses: Option<std::collections::BTreeMap<String, String>>,
    /// 作业任务目录策略："all"（默认）或 "substantive"。学习通作业说明
    /// 不含正文，"substantive" 下只为未交作业建目录，已交的空壳目录不再产生。
    pub work_vault_task_dirs: Option<String>,
    /// enc 样本库路径（科研线）；缺省开在 /data。
    pub enc_sample_path: Option<String>,
}

impl ChaoxingSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 60;
    /// 作业轮询间隔（秒）默认值；作业列表请求重，务必低频。
    pub const DEFAULT_WORK_POLL_SECS: u64 = 1800;
    /// 临期提醒窗口（天）默认值。
    pub const DEFAULT_WORK_DUE_DAYS: i64 = 3;
    /// cookie 持久化路径默认值。
    pub const DEFAULT_COOKIE_PATH: &str = ".ru41/chaoxing/cookies.json";
    /// Vault 学期目录默认值——必须与 sources.zhihuishu 的缺省一致，
    /// 两个平台的作业才聚合到同一目录（作业总表.md 也落在这里）。
    pub const DEFAULT_VAULT_COURSE_DIR: &str = "01-Courses-课程/2026-Fall-大一上";

    /// 由 [sources.chaoxing] 解析出运行时配置；默认值与凭证兜底只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<ChaoxingConfig> {
        let phone = std::env::var("RU41_XXT_PHONE")
            .ok()
            .or_else(|| self.phone.clone())
            .filter(|v| !v.trim().is_empty())
            .context("chaoxing phone required: RU41_XXT_PHONE or sources.chaoxing.phone")?;
        let password = std::env::var("RU41_XXT_PASSWORD")
            .ok()
            .or_else(|| self.password.clone())
            .filter(|v| !v.trim().is_empty())
            .context(
                "chaoxing password required: RU41_XXT_PASSWORD or sources.chaoxing.password",
            )?;
        Ok(ChaoxingConfig {
            phone,
            password,
            cookie_path: self
                .cookie_path
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_COOKIE_PATH.into())
                .into(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify_target,
            auto_sign: self.auto_sign.unwrap_or(false),
            default_location: match (
                self.default_lat,
                self.default_lon,
                self.default_addr.clone(),
            ) {
                (Some(lat), Some(lon), addr) => {
                    Some((lat, lon, addr.unwrap_or_else(|| "宿舍".to_owned())))
                }
                _ => None,
            },
            photo_path: self.photo_path.clone().map(std::path::PathBuf::from),
            work_poll_secs: self.work_poll_secs.unwrap_or(Self::DEFAULT_WORK_POLL_SECS),
            work_due_days: self.work_due_days.unwrap_or(Self::DEFAULT_WORK_DUE_DAYS),
            work_todo_sync: self.work_todo_sync.unwrap_or(true),
            work_vault_sync: self.work_vault_sync.unwrap_or(true),
            vault_course_dir: Some(
                self.vault_course_dir
                    .clone()
                    .unwrap_or_else(|| Self::DEFAULT_VAULT_COURSE_DIR.into()),
            ),
            vault_courses: self.vault_courses.clone().unwrap_or_default(),
            work_vault_task_dirs: crate::ports::vault::VaultTaskDirs::parse(
                self.work_vault_task_dirs.as_deref(),
            )?,
            enc_sample_path: Some(
                self.enc_sample_path
                    .clone()
                    .unwrap_or_else(|| "/data/chaoxing/enc-samples.jsonl".into())
                    .into(),
            ),
        })
    }

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        (self.phone.is_some() || std::env::var("RU41_XXT_PHONE").is_ok())
            && (self.password.is_some() || std::env::var("RU41_XXT_PASSWORD").is_ok())
    }
}
