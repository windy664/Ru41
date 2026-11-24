//! sidecar HTTP 客户端：查状态、触发刷课、重算目标清单。
//!
//! sidecar 是生产机上跑着的常驻容器（`ru41-sidecar`，带 Playwright + Xvfb），
//! 通过 `ru41-net` 用容器名直接访问。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::CourseBrushSection;

/// 一条目标课程（来自 sidecar 的 `targets.json`）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TargetCourse {
    pub title: Option<String>,
    pub engine: Option<String>,
    pub progress: Option<i64>,
    pub url: Option<String>,
}

/// 上一轮刷课的结果摘要。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RoundResult {
    pub title: Option<String>,
    pub watched: Option<usize>,
    pub failed: Option<usize>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct LastRound {
    pub generated_at: Option<String>,
    pub results: Vec<RoundResult>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ReportSummary {
    pub generated_at: Option<String>,
    pub login_ok: Option<bool>,
    pub courses: Option<usize>,
    pub tasks: Option<usize>,
    pub content_ready: Vec<String>,
}

/// `/status` 的响应。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Status {
    pub ok: bool,
    /// sidecar 那边是不是正在刷。
    pub busy: bool,
    /// sidecar 本地时间（CST）。
    pub now_local: Option<String>,
    pub timezone: Option<String>,
    /// **今天该不该刷** —— 判定在 sidecar 那边做（时区统一，见 serve.py）。
    pub due: bool,
    pub due_reason: Option<String>,
    pub daily_hour: Option<u32>,
    /// 上一轮刷完的时间（容器本地时区字符串）。
    pub generated_at: Option<String>,
    pub targets_generated_at: Option<String>,
    pub courses: Vec<TargetCourse>,
    pub last_round: LastRound,
    pub report: ReportSummary,
}

/// `POST /brush` 的响应。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BrushResult {
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub elapsed_secs: Option<u64>,
    pub error: Option<String>,
    pub status: Option<Status>,
}

/// 客户端错误。
#[derive(Debug)]
pub enum BrushError {
    Request(String),
    Status(String),
}

impl std::fmt::Display for BrushError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(message) => write!(formatter, "请求 sidecar 失败: {message}"),
            Self::Status(message) => write!(formatter, "sidecar 返回异常: {message}"),
        }
    }
}

impl std::error::Error for BrushError {}

#[derive(Clone)]
pub struct CourseBrushClient {
    base_url: String,
    client: reqwest::Client,
    brush_client: reqwest::Client,
}

impl CourseBrushClient {
    pub fn new(section: &CourseBrushSection) -> Result<Self, BrushError> {
        // ⚠️ 必须 no_proxy：sidecar 是 ru41-net 内的**容器名**地址
        // (http://ru41-sidecar:9100)。容器里注入了出站代理(HTTP_PROXY 指向 mihomo),
        // 而 NO_PROXY 只列了 localhost/127.0.0.1/ru41-browser/172.17.0.1 ——
        // 代理解析不了容器名, 请求过去拿到空响应, 报「EOF while parsing a value」。
        // 同款修法见 src/adapters/sources/competition/xfyun.rs(ru41-browser 同一个坑)。
        let client = reqwest::Client::builder()
            .timeout(section.timeout())
            .no_proxy()
            .build()
            .map_err(|error| BrushError::Request(error.to_string()))?;
        // 刷课是同步的长任务，用单独的 client + 超时。
        let brush_client = reqwest::Client::builder()
            .timeout(section.brush_timeout())
            .no_proxy()
            .build()
            .map_err(|error| BrushError::Request(error.to_string()))?;
        Ok(Self {
            base_url: section.base_url(),
            client,
            brush_client,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// 查当前状态（含「今天该不该刷」的判定）。
    pub async fn status(&self) -> Result<Status, BrushError> {
        let response = self
            .client
            .get(self.url("/status"))
            .send()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        let text = response
            .text()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        serde_json::from_str(&text).map_err(|error| BrushError::Status(format!("{error}: {text}")))
    }

    /// 触发一轮刷课（同步，等 sidecar 跑完才返回）。
    pub async fn brush(
        &self,
        total_minutes: u64,
        max_courses: u32,
    ) -> Result<BrushResult, BrushError> {
        let payload = serde_json::json!({
            "total_minutes": total_minutes,
            "max_courses": max_courses,
        });
        let response = self
            .brush_client
            .post(self.url("/brush"))
            .json(&payload)
            .send()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        let text = response
            .text()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        serde_json::from_str(&text).map_err(|error| BrushError::Status(format!("{error}: {text}")))
    }

    /// 让 sidecar 重算目标清单（跑 plan_targets.py）。
    pub async fn refresh_targets(&self) -> Result<BrushResult, BrushError> {
        let response = self
            .client
            .post(self.url("/targets/refresh"))
            .send()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        let text = response
            .text()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        serde_json::from_str(&text).map_err(|error| BrushError::Status(format!("{error}: {text}")))
    }

    /// 探活：容器在不在、网络通不通。
    pub async fn health(&self) -> Result<bool, BrushError> {
        let response = self
            .client
            .get(self.url("/health"))
            .send()
            .await
            .map_err(|error| BrushError::Request(error.to_string()))?;
        Ok(response.status().is_success())
    }
}

/// 便于测试：给一个固定超时构造 client。
pub fn test_client(base_url: &str, timeout: Duration) -> Result<CourseBrushClient, BrushError> {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .no_proxy()
        .build()
        .map_err(|error| BrushError::Request(error.to_string()))?;
    Ok(CourseBrushClient {
        base_url: base_url.trim_end_matches('/').to_owned(),
        brush_client: client.clone(),
        client,
    })
}
