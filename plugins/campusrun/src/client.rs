//! pace-forge 控制台的只读客户端。
//!
//! 只碰读接口：学期进度、跑步记录、单条记录详情、违规名单、账号登录态。
//! **不碰任何写接口**（`/api/run`、`/api/batch`、`/api/obs/fetch`…）——
//! 提交类操作不进这个插件，避免误触与账号风控。

use crate::config::CampusrunSection;
use crate::model::{
    AccountInfo, CheatStatus, Envelope, FencesData, PlanData, PointsData, RecordsData, RunData,
    RunRecord, RunRecordDetail, SemesterInfo, StartTimes, Verdict,
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
    /// 参数没过插件自己的门槛（距离/时长/配速）。
    Invalid(String),
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
            Self::Invalid(reason) => write!(f, "{reason}"),
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

    /// 通用 POST（`/api/plan`、`/api/run` 用）。
    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let token = self.token.as_ref().ok_or(CampusrunError::NoToken)?.clone();
        let url = self.url(path);
        let response = self
            .http
            .post(&url)
            .header("X-Pace-Token", token)
            .json(body)
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

    /// 最近一条记录的详情（含服务端给的逐条规则判定）。
    ///
    /// 规则判定是权威口径：门槛（最少多少公里/多少分钟）、点位顺序过没过，
    /// 都在这里——**不要写死**，各门课的要求不一样。
    pub async fn last_verdict(&self) -> std::result::Result<Option<Verdict>, CampusrunError> {
        let records = self.records().await?;
        let Some(record) = records
            .into_iter()
            .max_by_key(|item| item.start_time.unwrap_or_default())
        else {
            return Ok(None);
        };
        let detail = self.record(record.rrid).await?;
        Ok(Some(Verdict {
            rrid: record.rrid,
            filename: None,
            rules: detail.reason_list.clone().unwrap_or_default(),
            status_info: detail.status_info.clone(),
            min_distance_km: detail.sel_distance,
            pace_range: match (detail.speed_top, detail.speed_bottom) {
                (Some(top), Some(bottom)) => Some((top, bottom)),
                _ => None,
            },
        }))
    }

    /// 账号登录态（姓名/手机号不在渲染里回显）。
    pub async fn account(&self) -> Result<AccountInfo> {
        self.get("/api/me").await
    }

    /// 补跑窗口与限制。
    pub async fn start_times(&self) -> Result<StartTimes> {
        self.get("/api/starttimes").await
    }

    /// 补跑窗口与限制（带查询参数）。
    ///
    /// 起跑时刻**由服务端算**：控制台的 `resolveStartAt()` 就是这么调的
    /// （`mode=rand|at|ago`、`days`、`hhmm`、`minutes`、`dur`），
    /// 返回 `startMs` 直接当起跑时刻用。插件不自己重造一套随机规则。
    pub async fn start_times_query(&self, query: &str) -> Result<StartTimes> {
        let query = query.trim();
        let path = if query.is_empty() {
            "/api/starttimes".to_owned()
        } else {
            format!("/api/starttimes?{query}")
        };
        self.get(&path).await
    }

    /// 必到点（打卡点）。
    pub async fn points(&self) -> Result<PointsData> {
        self.get("/api/points").await
    }

    /// 围栏多边形。
    pub async fn fences(&self) -> Result<FencesData> {
        self.get("/api/fences").await
    }

    /// 预演：生成轨迹 + 八类自检，**不提交**。
    pub async fn plan(&self, request: &PlanRequest) -> Result<PlanData> {
        self.post_json("/api/plan", &request.to_body()).await
    }

    /// 真正提交一条记录（写操作！调用前必须过插件自己的闸）。
    pub async fn run(&self, request: &PlanRequest) -> Result<RunData> {
        self.post_json("/api/run", &request.to_body()).await
    }
}

/// 一次跑动的参数（与 pace-forge Web 控制台的请求体同构）。
#[derive(Debug, Clone)]
pub struct PlanRequest {
    /// 距离（公里）。
    pub distance_km: f64,
    /// 时长（分钟）。
    pub duration_min: f64,
    /// 跑法：1 = 记分跑（校园跑）。
    pub run_mode: i64,
    /// 运动类型：1 = 校园跑。
    pub sport_type: i64,
    /// 轨迹模式：free = 自动规划。
    pub mode: String,
    /// 道次（仅操场模式有意义）。
    pub lane: u8,
    /// 补跑起始时刻（毫秒）；None = 现在起算。
    pub start_at_ms: Option<i64>,
    /// 海拔下限（米）。
    pub alt_min: Option<f64>,
    /// 海拔上限（米）。
    pub alt_max: Option<f64>,
}

impl PlanRequest {
    /// 校园跑（记分跑）的参数：自动规划 + 围栏；起跑时刻与海拔按配置带。
    pub fn campusrun(params: &crate::submit::RunParams) -> Self {
        Self {
            distance_km: params.distance_km,
            duration_min: params.duration_min,
            run_mode: 1,
            sport_type: 1,
            mode: "free".to_owned(),
            lane: params.lane,
            start_at_ms: params.start_at_ms,
            alt_min: params.alt_range.map(|range| range.0),
            alt_max: params.alt_range.map(|range| range.1),
        }
    }

    /// 请求体（字段名与 Web 控制台一致）。
    pub fn to_body(&self) -> serde_json::Value {
        let mut body = serde_json::json!({
            "distance": self.distance_km,
            "duration": self.duration_min,
            "runMode": self.run_mode,
            "sportType": self.sport_type,
            "mode": self.mode,
            "lane": self.lane,
        });
        if let Some(start) = self.start_at_ms {
            body["startAtMs"] = serde_json::json!(start);
        }
        // 海拔区间：两个都填了才发（与 Web 控制台的 altBody 同规则）。
        if let (Some(lo), Some(hi)) = (self.alt_min, self.alt_max)
            && hi > lo
        {
            body["altMin"] = serde_json::json!(lo);
            body["altMax"] = serde_json::json!(hi);
        }
        body
    }
}
