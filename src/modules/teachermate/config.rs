use anyhow::{Context, Result};
use serde::Deserialize;

/// 微助教签到监控运行时配置（由 TeachermateSection::resolved 解析产出）。
#[derive(Debug, Clone)]
pub struct TeachermateConfig {
    /// 微信服务号网页 openid（32 位串）；学生侧唯一凭证，人工从服务号链接提取。
    pub openid: String,
    /// openid 持久化路径（bind 命令写入）；环境变量与配置文件优先于文件。
    pub openid_path: std::path::PathBuf,
    pub interval_secs: u64,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
    /// 自动签到：poll 发现待签活动时当场尝试（普通/GPS；二维码 v1 只提醒现场扫）。
    pub auto_sign: bool,
    /// GPS 签到默认坐标（如宿舍/教学楼）；None 时 GPS 也发普通载荷（平台校验弱）。
    pub default_location: Option<(f64, f64)>,
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
    /// 微助教课程名 → Vault 课程目录名映射。
    pub vault_courses: std::collections::BTreeMap<String, String>,
    /// 作业任务目录策略；substantive 下只为未交作业建目录（说明页无正文）。
    pub work_vault_task_dirs: crate::ports::vault::VaultTaskDirs,
}

/// 微助教签到监控配置；teachermate poll/run/sign/bind 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TeachermateSection {
    /// 服务号网页 openid（建议直接用 `teachermate bind <链接>` 落盘，勿提交）。
    pub openid: Option<String>,
    /// openid 持久化路径；默认 .ru41/teachermate/openid。
    pub openid_path: Option<String>,
    /// teachermate run 的轮询间隔（秒），默认 60（签到窗口常为 1-2 分钟，
    /// 太疏会漏签；active_signs 请求极轻，介意风控可调大）。
    pub interval_secs: Option<u64>,
    /// 检测到待签活动是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// 自动签到：poll 发现待签活动时当场尝试（普通/GPS；二维码 v1 只提醒
    /// 现场扫，替扫待 qrUrl 样本验证，见 recon 实测记录 3）。默认 false。
    pub auto_sign: Option<bool>,
    /// GPS 签到默认纬度（如宿舍）。
    pub default_lat: Option<f64>,
    /// GPS 签到默认经度。
    pub default_lon: Option<f64>,
    /// 作业轮询间隔（秒），默认 1800（截止粒度为天，30 分钟足够）。
    pub work_poll_secs: Option<u64>,
    /// 截止前 N 天开始临期提醒，默认 3。
    pub work_due_days: Option<i64>,
    /// 未交作业自动进待办、平台已交自动回勾；默认 false。
    pub work_todo_sync: Option<bool>,
    /// 作业清单同步进 Obsidian 课程目录 + 跨平台作业总表；默认 false。
    pub work_vault_sync: Option<bool>,
    /// Vault 内课程所在学期目录（work_vault_sync 开启时必配）。
    pub vault_course_dir: Option<String>,
    /// 微助教课程名 → Vault 课程目录名映射（缺省用课程原名）。
    pub vault_courses: Option<std::collections::BTreeMap<String, String>>,
    /// 作业任务目录策略："all"（默认）或 "substantive"（只为未交作业建目录）。
    pub work_vault_task_dirs: Option<String>,
}

impl TeachermateSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 60;
    /// 作业轮询间隔（秒）默认值。
    pub const DEFAULT_WORK_POLL_SECS: u64 = 1800;
    /// 临期提醒窗口（天）默认值。
    pub const DEFAULT_WORK_DUE_DAYS: i64 = 3;
    /// openid 持久化路径默认值。
    pub const DEFAULT_OPENID_PATH: &str = ".ru41/teachermate/openid";
    /// openid 环境变量名。
    pub const ENV_OPENID: &str = "RU41_WZJ_OPENID";

    /// openid 解析顺序：环境变量 > 配置 > openid_path 文件（bind 命令写入）。
    pub fn openid_from_all(&self) -> Option<String> {
        std::env::var(Self::ENV_OPENID)
            .ok()
            .or_else(|| self.openid.clone())
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().to_owned())
            .or_else(|| {
                let path = self.openid_path_or_default();
                std::fs::read_to_string(path)
                    .ok()
                    .map(|text| text.trim().to_owned())
                    .filter(|v| !v.is_empty())
            })
    }

    /// openid 持久化路径；bind 命令在凭证缺失时也要能写入，故不走 resolved()。
    pub fn openid_path_or_default(&self) -> std::path::PathBuf {
        self.openid_path
            .clone()
            .unwrap_or_else(|| Self::DEFAULT_OPENID_PATH.into())
            .into()
    }

    /// 由 [sources.teachermate] 解析出运行时配置；默认值与凭证兜底只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<TeachermateConfig> {
        let openid = self.openid_from_all().with_context(|| {
            "teachermate openid required: `teachermate bind <链接>` 落盘、\
             或 RU41_WZJ_OPENID / sources.teachermate.openid"
        })?;
        Ok(TeachermateConfig {
            openid,
            openid_path: self.openid_path_or_default(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            notify_target,
            auto_sign: self.auto_sign.unwrap_or(false),
            default_location: match (self.default_lat, self.default_lon) {
                (Some(lat), Some(lon)) => Some((lat, lon)),
                _ => None,
            },
            work_poll_secs: self.work_poll_secs.unwrap_or(Self::DEFAULT_WORK_POLL_SECS),
            work_due_days: self.work_due_days.unwrap_or(Self::DEFAULT_WORK_DUE_DAYS),
            work_todo_sync: self.work_todo_sync.unwrap_or(false),
            work_vault_sync: self.work_vault_sync.unwrap_or(false),
            vault_course_dir: self.vault_course_dir.clone(),
            vault_courses: self.vault_courses.clone().unwrap_or_default(),
            work_vault_task_dirs: crate::ports::vault::VaultTaskDirs::parse(
                self.work_vault_task_dirs.as_deref(),
            )?,
        })
    }

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        self.openid_from_all().is_some()
    }
}

/// 从粘贴内容提取 openid：支持完整服务号链接（openid=...，大小写不敏感）
/// 或裸 32 位串；其余输入返回 None。
pub fn extract_openid(raw: &str) -> Option<String> {
    static OPENID_RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)openid=([A-Za-z0-9]+)").unwrap());
    let raw = raw.trim();
    if let Some(captures) = OPENID_RE.captures(raw) {
        return captures.get(1).map(|m| m.as_str().to_owned());
    }
    let is_token = (16..=64).contains(&raw.len())
        && !raw.is_empty()
        && raw.chars().all(|c| c.is_ascii_alphanumeric());
    is_token.then(|| raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_openid_from_link_and_bare_token() {
        let link = "https://v18.teachermate.cn/wechat-pro-ssr/student/sign?openid=abc123XYZ_-456def78901234567890&foo=1";
        // 下划线不属于 openid 字符集：链接场景取到 & 截断为止
        assert_eq!(extract_openid(link).as_deref(), Some("abc123XYZ"));
        assert_eq!(
            extract_openid("ABCDEF0123456789abcdef0123456789").as_deref(),
            Some("ABCDEF0123456789abcdef0123456789")
        );
        assert_eq!(
            extract_openid("openId=deadbeefdeadbeef").as_deref(),
            Some("deadbeefdeadbeef")
        );
        assert_eq!(extract_openid("不是链接"), None);
        assert_eq!(extract_openid(""), None);
    }
}
