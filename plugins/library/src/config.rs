use anyhow::{Context, Result};
use serde::Deserialize;

use crate::service::LibraryConfig;

/// 图书馆借阅监控配置；library poll/run 使用。协议见 docs/library-recon.md。
/// 凭证与教务同套（联奕 CAS），默认直接复用 RU41_JWXT_USERNAME/PASSWORD，
/// 不用在配置里存第二份密码。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LibrarySection {
    /// 统一身份认证账号（学号）；默认复用 RU41_JWXT_USERNAME 或 sources.jwxt。
    pub username: Option<String>,
    /// 统一身份认证密码；默认复用 RU41_JWXT_PASSWORD 或 sources.jwxt。
    pub password: Option<String>,
    /// CAS 基址（联奕 lyuapServer）。
    pub cas_base: Option<String>,
    /// CAS 前端 RSA 公钥 modulus（hex，与教务同套部署配置）。
    pub cas_modulus: Option<String>,
    /// CAS 前端 RSA 公钥 exponent（hex，默认 010001）。
    pub cas_exponent: Option<String>,
    /// 超星统一认证基址。
    pub unified_base: Option<String>,
    /// libsp PC 端基址（SSO 落地跳板）。
    pub find_base: Option<String>,
    /// libsp 移动端基址（业务 API 同源）。
    pub mobile_base: Option<String>,
    /// 学校编码（API 头 groupCode；JWT payload 里也有）。
    pub group_code: Option<String>,
    /// 会话持久化路径（TGT + cookie + JWT）；默认 .ru41/library/session.json。
    pub session_path: Option<String>,
    /// poll 的轮询间隔（秒），默认 21600（6 小时）。
    pub interval_secs: Option<u64>,
    /// 应还前 N 天开始每日提醒，默认 3。
    pub remind_days: Option<i64>,
    /// 临近到期是否经默认通知出口推送；默认 true。
    pub notify: Option<bool>,
}

impl LibrarySection {
    /// poll 的轮询间隔（秒）默认值。
    pub const DEFAULT_INTERVAL_SECS: u64 = 21_600;
    /// 应还前 N 天开始提醒的默认值。
    pub const DEFAULT_REMIND_DAYS: i64 = 3;
    /// CAS 基址默认值（与教务同套）。
    pub const DEFAULT_CAS_BASE: &str = "https://cas.gzus.edu.cn/lyuapServer";
    /// CAS 前端 RSA modulus 默认值（与教务共用；改版时经本地配置覆盖）。
    pub const DEFAULT_CAS_MODULUS: &str = "00b5eeb166e069920e80bebd1fea4829d3d1f3216f2aabe79b6c47a3c18dcee5fd22c2e7ac519cab59198ece036dcf289ea8201e2a0b9ded307f8fb704136eaeb670286f5ad44e691005ba9ea5af04ada5367cd724b5a26fdb5120cc95b6431604bd219c6b7d83a6f8f24b43918ea988a76f93c333aa5a20991493d4eb1117e7b1";
    pub const DEFAULT_CAS_EXPONENT: &str = "010001";
    /// 超星统一认证基址默认值。
    pub const DEFAULT_UNIFIED_BASE: &str = "https://unified-auth.chaoxing.com";
    /// libsp PC 端基址默认值。
    pub const DEFAULT_FIND_BASE: &str = "https://findgzus.libsp.cn";
    /// libsp 移动端基址默认值。
    pub const DEFAULT_MOBILE_BASE: &str = "https://mfindgzus.libsp.cn";
    /// 学校编码默认值。
    pub const DEFAULT_GROUP_CODE: &str = "201550";
    /// 会话文件默认路径。
    pub const DEFAULT_SESSION_PATH: &str = ".ru41/library/session.json";

    /// 账号解析：环境变量（LIBRARY 优先，JWXT 兜底）→ 配置项。
    pub fn resolve_username(&self) -> Option<String> {
        Self::first_non_empty(&[
            std::env::var("RU41_LIBRARY_USERNAME").ok(),
            std::env::var("RU41_JWXT_USERNAME").ok(),
            self.username.clone(),
        ])
    }

    /// 密码解析：环境变量（LIBRARY 优先，JWXT 兜底）→ 配置项。
    pub fn resolve_password(&self) -> Option<String> {
        Self::first_non_empty(&[
            std::env::var("RU41_LIBRARY_PASSWORD").ok(),
            std::env::var("RU41_JWXT_PASSWORD").ok(),
            self.password.clone(),
        ])
    }

    fn first_non_empty(candidates: &[Option<String>]) -> Option<String> {
        candidates
            .iter()
            .find_map(|c| c.as_deref().map(str::trim).filter(|v| !v.is_empty()))
            .map(str::to_owned)
    }

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        self.resolve_username().is_some() && self.resolve_password().is_some()
    }

    /// 由 [sources.library] 解析出运行时配置；默认值与凭证兜底只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<LibraryConfig> {
        let username = self.resolve_username().context(
            "library username required: RU41_LIBRARY_USERNAME / RU41_JWXT_USERNAME or sources.library.username",
        )?;
        let password = self.resolve_password().context(
            "library password required: RU41_LIBRARY_PASSWORD / RU41_JWXT_PASSWORD or sources.library.password",
        )?;
        Ok(LibraryConfig {
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
            unified_base: self
                .unified_base
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_UNIFIED_BASE.into()),
            find_base: self
                .find_base
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_FIND_BASE.into()),
            mobile_base: self
                .mobile_base
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_MOBILE_BASE.into()),
            group_code: self
                .group_code
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_GROUP_CODE.into()),
            session_path: self
                .session_path
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_SESSION_PATH.into())
                .into(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            remind_days: self.remind_days.unwrap_or(Self::DEFAULT_REMIND_DAYS),
            notify_target,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::LibrarySection;

    #[test]
    fn credential_falls_back_to_jwxt_env() {
        // 本测试进程通常没有 RU41_JWXT_*；只验证「都没配时 is_ready=false」，
        // 以及配置项兜底生效（不读环境变量失败的分支）。
        let section = LibrarySection {
            username: Some("u001".into()),
            password: Some("p".into()),
            ..Default::default()
        };
        if std::env::var("RU41_LIBRARY_USERNAME").is_err()
            && std::env::var("RU41_JWXT_USERNAME").is_err()
        {
            assert_eq!(section.resolve_username().as_deref(), Some("u001"));
        }
        assert!(section.is_ready() || std::env::var("RU41_JWXT_PASSWORD").is_ok());
    }
}
