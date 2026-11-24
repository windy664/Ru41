//! 自动刷课附属的配置段。
//!
//! 刷课本身跑在 sidecar（生产机上的 `ru41-sidecar` 常驻容器，带 Playwright + Xvfb），
//! 本插件只负责**调度、汇报与通知**：
//!   - 什么时候该刷：问 sidecar 的 `/status`（它那边统一算时区与「今天刷过没」）
//!   - 刷什么：sidecar 读自己的 `targets.json`（挑课是 `plan_targets.py` 的事）
//!   - 刷完：拿回结果，渲染成报告，走 ru41 的通知出口
//!
//! ⚠️ 为什么刷课不写在插件里：平台的「资源已完成」标记是**浏览器里的前端 JS** 上报的，
//! 请求体还是加密的（`secretStr`），没有真实浏览器点就点不亮。所以浏览器那一段留在 sidecar。

use serde::Deserialize;
use std::time::Duration;

/// `[course-brush]`（plugins.d/course-brush/config.toml 里的段名 = 插件名）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CourseBrushSection {
    /// sidecar 服务基址。
    ///
    /// 默认 `http://ru41-sidecar:9100` —— 两个容器在同一个 `ru41-net` 网络里，
    /// 容器名能直接解析（跟 pace-forge 那种「不在同一网络、只能用宿主网关」的情况不同）。
    pub base_url: Option<String>,
    /// 普通请求超时（秒），默认 15。
    pub timeout_secs: Option<u64>,
    /// **刷课请求**的超时（秒），默认 3 小时。
    ///
    /// 刷一轮是同步的：sidecar 跑完整个 `brush_courses.py` 才返回，
    /// 期间要开浏览器、播视频、浏览文档 —— 十几分钟到几小时都正常。
    /// 用普通请求的超时会被误判成失败。
    pub brush_timeout_secs: Option<u64>,
    /// 轮询间隔（秒），默认 3600（每小时看一眼）。
    ///
    /// 间隔只是「多久问一次该不该刷」；真正的开工时间由 sidecar 那边决定
    /// （每天 19:00 CST 之后、且今天还没刷过）。
    pub interval_secs: Option<u64>,
    /// 单轮预算（分钟），默认 120 —— 跟原来 systemd 那轮一致。
    pub max_minutes: Option<u64>,
    /// 每轮最多刷几门，默认 3。
    pub max_courses: Option<u32>,
    /// 是否发通知（默认发）。
    ///
    /// 「今天不用刷」这种日常状态**不发**（是噪音），只有真刷了或出错了才发。
    pub notify: Option<bool>,
}

impl CourseBrushSection {
    /// 默认基址：ru41-net 上的 sidecar 容器名。
    pub const DEFAULT_BASE_URL: &str = "http://ru41-sidecar:9100";
    pub const DEFAULT_TIMEOUT_SECS: u64 = 15;
    pub const DEFAULT_BRUSH_TIMEOUT_SECS: u64 = 3 * 3600;
    pub const DEFAULT_INTERVAL_SECS: u64 = 3600;
    pub const DEFAULT_MAX_MINUTES: u64 = 120;
    pub const DEFAULT_MAX_COURSES: u32 = 3;

    /// 基址（去掉尾部斜杠）。
    pub fn base_url(&self) -> String {
        let raw = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(Self::DEFAULT_BASE_URL);
        raw.trim_end_matches('/').to_owned()
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.timeout_secs
                .unwrap_or(Self::DEFAULT_TIMEOUT_SECS)
                .max(1),
        )
    }

    pub fn brush_timeout(&self) -> Duration {
        Duration::from_secs(
            self.brush_timeout_secs
                .unwrap_or(Self::DEFAULT_BRUSH_TIMEOUT_SECS)
                .max(60),
        )
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs(
            self.interval_secs
                .unwrap_or(Self::DEFAULT_INTERVAL_SECS)
                .max(60),
        )
    }

    pub fn max_minutes(&self) -> u64 {
        self.max_minutes.unwrap_or(Self::DEFAULT_MAX_MINUTES).max(1)
    }

    pub fn max_courses(&self) -> u32 {
        self.max_courses.unwrap_or(Self::DEFAULT_MAX_COURSES).max(1)
    }

    /// 发通知吗（默认发）。
    pub fn notify(&self) -> bool {
        self.notify.unwrap_or(true)
    }

    /// 能不能用：没配基址就没什么可调用的（schedule 里据此显示未就绪原因）。
    pub fn is_ready(&self) -> bool {
        !self.base_url().is_empty()
    }
}
