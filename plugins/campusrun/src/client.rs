//! pace-forge 控制台的只读客户端。
//!
//! 只碰读接口：学期进度、跑步记录、单条记录详情、违规名单、账号登录态。
//! **不碰任何写接口**（`/api/run`、`/api/batch`、`/api/obs/fetch`…）——
//! 提交类操作不进这个插件，避免误触与账号风控。

use crate::config::CampusrunSection;
use crate::model::{
    AccountInfo, CheatStatus, Envelope, RecordsData, RunRecord, RunRecordDetail, SemesterInfo,
};
use serde::de::DeserializeOwned;
use std::fmt;
use std::time::Duration;

/// 插件的错误类型：都要能直接说给人听。
#[derive(Debug)]
pub enum CampusrunError {
    /// 没配令牌：控制台所有 /api/* 都要 `X-Pace-Token`。
    NoToken,
    /// 连不上控制台（容器没起、base_url 写错、两个 compose 网络不通）。
    Unreachable { url: String, reason: String },
    /// 令牌不对（401/403）。
    Unauthorized,
    /// 控制台返回业务错误。
    Api(String),
    /// 响应结构对不上（厂商改了字段）。
    Decode(String),
}

impl fmt::Display for CampusrunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoToken => write!(
                f,
                "没有 pace-forge 控制台令牌：设环境变量 PACE_FORGE_TOKEN，\
                 或在 [sources.campusrun] 里写 token"
            ),
            Self::Unreachable { url, reason } => write!(
                f,
                "连不上 pace-forge 控制台（{url}）：{reason}；\
                 检查容器是否在跑、base_url 是否指向宿主的发布端口"
            ),
            Self::Unauthorized => write!(f, "控制台令牌不对（401/403）：核对 PACE_FORGE_TOKEN"),
            Self::Api(message) => write!(f, "pace-forge 返回错误：{message}"),
            Self::Decode(reason) => write!(f, "pace-forge 返回结构对不上：{reason}"),
        }
    }
}

impl std::error::Error for CampusrunError {}

pub type Result<T> = std::result::Result<T, CampusrunError>;

/// 控制台只读客户端。
#[derive(Clone)]
pub struct CampusrunClient {
    http: reqwest::Client,
    base_url: String,
    token: Option<String>,
}

impl CampusrunClient {
    /// 按配置装配客户端。没令牌也允许构造（请求会立刻报 NoToken）。
    pub fn new(section: &CampusrunSection) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(section.timeout())
            .build()
            .map_err(|error| CampusrunError::Unreachable {
                url: section.base_url(),
                reason: error.to_string(),
            })?;
        Ok(Self {
            http,
            base_url: section.base_url(),
            token: section.token(),
        })
    }

    /// 拼完整 URL（测试用得上）。
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// 令牌是否已就位。
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// 发一个 GET，解 `{"ok":..,"data":..}` 信封。
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let token = self.token.as_ref().ok_or(CampusrunError::NoToken)?.clone();
        let url = self.url(path);
        let response = self
            .http
            .get(&url)
            .header("X-Pace-Token", token)
            .send()
            .await
            .map_err(|error| CampusrunError::Unreachable {
                url: url.clone(),
                reason: error.to_string(),
            })?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(CampusrunError::Unauthorized);
        }
        let envelope: Envelope<T> = response
            .json()
            .await
            .map_err(|error| CampusrunError::Decode(error.to_string()))?;
        if !envelope.ok {
            return Err(CampusrunError::Api(
                envelope.error.unwrap_or_else(|| "未知错误".to_owned()),
            ));
        }
        envelope
            .data
            .ok_or_else(|| CampusrunError::Decode("返回里没有 data".to_owned()))
    }

    /// 通用 GET（供测试与将来只读扩展用）。
    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        self.get(path).await
    }

    /// 学期进度（总/有效里程、目标、剩余天数）。
    pub async fn semester(&self) -> Result<SemesterInfo> {
        self.get("/api/semester").await
    }

    /// 跑步记录列表。
    pub async fn records(&self) -> Result<Vec<RunRecord>> {
        let data: RecordsData = self.get("/api/records").await?;
        Ok(data.records)
    }

    /// 单条记录详情（含点位配速与规则最小里程）。
    pub async fn record(&self, rrid: i64) -> Result<RunRecordDetail> {
        self.get(&format!("/api/record/{rrid}")).await
    }

    /// 违规名单自查状态。
    pub async fn cheat(&self) -> Result<CheatStatus> {
        self.get("/api/cheat").await
    }

    /// 账号登录态（姓名/手机号不在渲染里回显）。
    pub async fn account(&self) -> Result<AccountInfo> {
        self.get("/api/me").await
    }
}
