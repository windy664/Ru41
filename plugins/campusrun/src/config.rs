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
}

impl CampusrunSection {
    pub const DEFAULT_BASE_URL: &str = "http://172.22.0.1:9999";
    pub const DEFAULT_TOKEN_ENV: &str = "PACE_FORGE_TOKEN";
    pub const DEFAULT_TIMEOUT_SECS: u64 = 15;

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

    /// 能不能用：没令牌就没什么可查的（schedule 里据此显示未就绪原因）。
    pub fn is_ready(&self) -> bool {
        self.token().is_some()
    }
}
