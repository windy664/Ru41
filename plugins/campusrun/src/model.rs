//! pace-forge 控制台 API 的响应结构（只读部分）。
//!
//! 字段名与 pace-forge 0.1.5 实测一致（`summaryRaw` / `goalRaw` / `validDis`…）。
//! 数值字段一律用 Option：厂商改字段时宁可缺值也不要解析失败。

use serde::Deserialize;

/// 控制台的统一信封：`{"ok": true, "data": {...}}` 或 `{"ok": false, "error": "..."}`。
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "T: serde::de::DeserializeOwned"))]
pub struct Envelope<T> {
    pub ok: bool,
    #[serde(default)]
    pub data: Option<T>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `/api/semester` 的 data。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemesterInfo {
    /// 学期累计（总里程 / 有效里程 / 次数）。
    pub summary_raw: SemesterSummary,
    /// 服务端算好的目标与剩余天数。
    #[serde(default)]
    pub goal_raw: Option<SemesterGoalRaw>,
    /// 目标视图：目标距离、目标次数、剩余天数、是否在学期内。
    #[serde(default)]
    pub goal: Option<SemesterGoal>,
}

/// 学期累计（原始字段）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemesterSummary {
    /// 本学期总里程（米，含无效的）。
    #[serde(default)]
    pub semester_dis: Option<f64>,
    /// 本学期有效里程（米）——只有这部分计入体育成绩。
    #[serde(default)]
    pub semester_valid_dis: Option<f64>,
    /// 学期名，如「2026-2027-1体能训练Ⅰ」。
    #[serde(default)]
    pub sname: Option<String>,
    /// 总次数。
    #[serde(default)]
    pub semester_count: Option<i64>,
    /// 有效次数。
    #[serde(default)]
    pub semester_valid_count: Option<i64>,
    /// 有效比例（0..1）。
    #[serde(default)]
    pub valid_ratio: Option<f64>,
    /// 服务端拼好的一句话摘要。
    #[serde(default)]
    pub text: Option<String>,
}

/// 学期目标（原始字段）。
///
/// ⚠️ pace-forge 自带的 CLI `semester` 子命令把这里的目标打成了「0.0km / 0 次」
/// ——它读错了字段。真实目标在 `semesterGoal` / `semesterValid` / `goalMethod`，
/// 插件一律以这里为准。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemesterGoalRaw {
    /// 学期目标次数（1 = 要求跑够 1 次）。
    #[serde(default)]
    pub semester_goal: Option<i64>,
    /// 已完成的有效次数。
    #[serde(default)]
    pub semester_valid: Option<i64>,
    /// 达标口径（2 = 按次数/距离口径，厂商定义）。
    #[serde(default)]
    pub goal_method: Option<i64>,
    /// 学期开始时间（毫秒）。
    #[serde(default)]
    pub start_date: Option<i64>,
    /// 学期结束时间（毫秒）。
    #[serde(default)]
    pub end_date: Option<i64>,
    #[serde(default)]
    pub sname: Option<String>,
}

/// 学期目标（视图）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemesterGoal {
    /// 目标距离（km；与记录详情里的最小单次里程 selDistance 同值）。
    #[serde(default)]
    pub target_dis: Option<f64>,
    /// 目标次数（0 = 不计次）。
    #[serde(default)]
    pub target_count: Option<i64>,
    /// 学期剩余天数。
    #[serde(default)]
    pub days_left: Option<i64>,
    /// 现在是否在学期内。
    #[serde(default)]
    pub in_term: Option<bool>,
}

/// `/api/records` 的 data。
#[derive(Debug, Clone, Deserialize)]
pub struct RecordsData {
    #[serde(default)]
    pub records: Vec<RunRecord>,
}

/// 一条跑步记录（列表项）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecord {
    /// 记录 id（服务端下发）。
    pub rrid: i64,
    /// 本次记录的 uuid（台账里用它做幂等键）。
    #[serde(default)]
    pub uuid: Option<String>,
    /// 运动类型（1 = 校园跑/记分跑）。
    #[serde(default)]
    pub sport_type: Option<i64>,
    /// 总距离（米）。
    #[serde(default)]
    pub total_dis: Option<f64>,
    /// 总时长（秒）。
    #[serde(default)]
    pub total_time: Option<f64>,
    /// 有效距离（米）——计入成绩的部分。
    #[serde(default)]
    pub valid_dis: Option<f64>,
    /// 有效时长（秒）。
    #[serde(default)]
    pub valid_time: Option<f64>,
    /// 开始时间（毫秒）。
    #[serde(default)]
    pub start_time: Option<i64>,
    /// 结束时间（毫秒）。
    #[serde(default)]
    pub stop_time: Option<i64>,
    /// 是否完成。
    #[serde(default)]
    pub complete: Option<bool>,
    /// 服务端状态码（0 = 正常，非 0 表示被标记）。
    #[serde(default)]
    pub status: Option<i64>,
    /// 申诉状态（非 0 = 有申诉记录）。
    #[serde(default)]
    pub appeal_status: Option<i64>,
    /// 步频校验状态（非 0 = 步频被判异常）。
    #[serde(default)]
    pub step_freq_status: Option<i64>,
    /// 平均步频（步/分）。
    #[serde(default)]
    pub avg_step_freq: Option<i64>,
    /// 距离（km，服务端算好的）。
    #[serde(default)]
    pub distance_km: Option<f64>,
}

/// 一条跑步记录的详情（`/api/record/{rrid}`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecordDetail {
    pub rrid: i64,
    /// 点位配速（min/km，不是 km/h）。
    #[serde(default)]
    pub speed: Option<f64>,
    /// 规则要求的最小单次里程（km）。
    #[serde(default)]
    pub sel_distance: Option<f64>,
    #[serde(default)]
    pub total_dis: Option<f64>,
    #[serde(default)]
    pub total_time: Option<f64>,
    #[serde(default)]
    pub valid_dis: Option<f64>,
    #[serde(default)]
    pub start_time: Option<i64>,
    #[serde(default)]
    pub stop_time: Option<i64>,
    #[serde(default)]
    pub complete: Option<bool>,
    #[serde(default)]
    pub status: Option<i64>,
    /// 未完成原因码（complete=false 时有值）。
    #[serde(default)]
    pub un_complete_reason: Option<i64>,
}

/// `/api/cheat` 的 data：违规名单自查结果。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheatStatus {
    /// no_record | flagged | unknown | stale。
    pub status: String,
    /// 服务端给的中文标签。
    #[serde(default)]
    pub label: Option<String>,
    /// 违规条数。
    #[serde(default)]
    pub count: Option<i64>,
    /// 标记原因。
    #[serde(default)]
    pub reason: Option<String>,
    /// 申诉状态。
    #[serde(default)]
    pub appeal: Option<String>,
    /// 标记时间。
    #[serde(default)]
    pub occurred_at: Option<String>,
    /// 关联的记录 id。
    #[serde(default)]
    pub rrid: Option<i64>,
    /// 查询时间（毫秒）。
    #[serde(default)]
    pub checked_at_ms: Option<i64>,
    /// 通报列表条数（不是违规数，是通报条数）。
    #[serde(default)]
    pub list_count: Option<i64>,
}

/// `/api/me` 的 data：账号信息。
///
/// ⚠️ 含姓名与手机号：**只用于判断登录态，渲染时不要回给用户**。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub school: Option<String>,
    /// 是否处于登录态。
    #[serde(default)]
    pub logged_in: Option<bool>,
}
