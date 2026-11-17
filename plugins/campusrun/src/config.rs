//! 校园跑附属的配置段。
//!
//! 数据来源是生产机上已部署的 pace-forge 控制台（运动世界校园的非官方 Rust 客户端），
//! 本插件**只读**它的 Web API：学期进度、跑步记录、违规状态。
//! 控制台令牌属密钥：优先用环境变量注入，别写进仓库。

use serde::Deserialize;
use std::time::Duration;

/// `[sources.campusrun]`（或 plugins.d/campusrun/config.toml 里的 `[campusrun]`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CampusrunSection {
    /// pace-forge 控制台基址，如 `http://172.22.0.1:9999`。
    ///
    /// 两个容器不在同一个 compose 网络，**容器名 `pace-forge` 解析不了**；
    /// 用宿主在 pace-forge 网络上的网关（172.22.0.1）或宿主 LAN IP 都通。
    pub base_url: Option<String>,
    /// 控制台令牌的环境变量名（默认 `PACE_FORGE_TOKEN`）。
    pub token_env: Option<String>,
    /// 控制台令牌本体。优先于环境变量；只允许写在 gitignore 的本地 config.toml 里。
    pub token: Option<String>,
    /// 单次请求超时（秒），默认 15。
    pub timeout_secs: Option<u64>,
    /// **是否允许真的提交跑步记录**（默认 false）。
    ///
    /// 提交是不可逆的写操作：跑上去就计入体育成绩，多交还会被风控盯上。
    /// 所以默认关着——只有显式打开、且用户在对话里明确要求时才可能提交。
    pub allow_submit: Option<bool>,
    /// 配速区间（分/km），默认 5.5~6.6（与 pace-forge Web 控制台的默认值一致）。
    pub pace_min: Option<f64>,
    /// 配速区间上限（分/km），默认 6.6。
    pub pace_max: Option<f64>,
    /// 校园跑单次最小里程（米），默认 1000（服务端 `minDistance`）。
    pub min_distance_m: Option<f64>,
    /// 校园跑单次最小时长（秒），默认 300（服务端 `minRunTime`）。
    pub min_duration_sec: Option<i64>,
    /// 提交后的静默窗口（秒），默认 600（工具 `flow::SILENCE_SEC`）。
    pub silence_secs: Option<i64>,
    /// 待确认提交的有效期（秒），默认 900。
    pub confirm_ttl_secs: Option<i64>,
}

impl CampusrunSection {
    pub const DEFAULT_BASE_URL: &str = "http://172.22.0.1:9999";
    pub const DEFAULT_TOKEN_ENV: &str = "PACE_FORGE_TOKEN";
    pub const DEFAULT_TIMEOUT_SECS: u64 = 15;
    /// 配速区间下限（分/km）——与 pace-forge Web 控制台的默认值一致。
    pub const DEFAULT_PACE_MIN: f64 = 5.5;
    /// 配速区间上限（分/km）。
    pub const DEFAULT_PACE_MAX: f64 = 6.6;
    /// 校园跑单次最小里程（米）——服务端 `minDistance`。
    pub const DEFAULT_MIN_DISTANCE_M: f64 = 1000.0;
    /// 校园跑单次最小时长（秒）——服务端 `minRunTime`。
    pub const DEFAULT_MIN_DURATION_SEC: i64 = 300;
    /// 提交后的静默窗口（秒）——工具 `flow::SILENCE_SEC`。
    pub const DEFAULT_SILENCE_SECS: i64 = 600;
    /// 待确认提交的有效期（秒）。
    pub const DEFAULT_CONFIRM_TTL_SECS: i64 = 900;

    /// 控制台基址（去掉尾部斜杠）。
    pub fn base_url(&self) -> String {
        let raw = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(Self::DEFAULT_BASE_URL);
        raw.trim_end_matches('/').to_owned()
    }

    /// 令牌环境变量名。
    pub fn token_env(&self) -> &str {
        self.token_env
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(Self::DEFAULT_TOKEN_ENV)
    }

    /// 令牌：配置里的 `token` 优先，否则读环境变量。
    pub fn token(&self) -> Option<String> {
        if let Some(token) = self
            .token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(token.to_owned());
        }
        std::env::var(self.token_env())
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    /// 请求超时。
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.timeout_secs
                .unwrap_or(Self::DEFAULT_TIMEOUT_SECS)
                .max(1),
        )
    }

    /// 允许提交吗（默认否）。
    pub fn allow_submit(&self) -> bool {
        self.allow_submit.unwrap_or(false)
    }

    /// 配速区间（分/km），返回 (下限, 上限)。
    pub fn pace_range(&self) -> (f64, f64) {
        let lo = self.pace_min.unwrap_or(Self::DEFAULT_PACE_MIN);
        let hi = self.pace_max.unwrap_or(Self::DEFAULT_PACE_MAX);
        (lo.min(hi), lo.max(hi))
    }

    /// 单次最小里程（米）。
    pub fn min_distance_m(&self) -> f64 {
        self.min_distance_m.unwrap_or(Self::DEFAULT_MIN_DISTANCE_M)
    }

    /// 单次最小时长（秒）。
    pub fn min_duration_sec(&self) -> i64 {
        self.min_duration_sec
            .unwrap_or(Self::DEFAULT_MIN_DURATION_SEC)
    }

    /// 静默窗口（秒）。
    pub fn silence_secs(&self) -> i64 {
        self.silence_secs.unwrap_or(Self::DEFAULT_SILENCE_SECS)
    }

    /// 待确认提交的有效期（秒）。
    pub fn confirm_ttl_secs(&self) -> i64 {
        self.confirm_ttl_secs
            .unwrap_or(Self::DEFAULT_CONFIRM_TTL_SECS)
    }

    /// 能不能用：没令牌就没什么可查的（schedule 里据此显示未就绪原因）。
    pub fn is_ready(&self) -> bool {
        self.token().is_some()
    }
}
