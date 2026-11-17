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
    /// 目标里程（**米**，不是次数）。
    #[serde(default)]
    pub semester_goal: Option<f64>,
    /// 已完成里程（米，服务端口径）。
    #[serde(default)]
    pub semester_valid: Option<f64>,
    /// 达标口径（2 = 按里程，厂商定义）。
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

/// 服务端给的**逐条规则判定**（记录详情里的 reasonList）——权威口径。
///
/// 每条规则带 complete（这条过没过）与 completeStatus。规则**每门课不同**：
/// 实测 2026-11-16 有过「最少要跑1.00公里 / 跑7分钟以上」，也有过
/// 「最少要跑1.50公里 / 跑6分钟以上」——所以门槛不能写死，要从这里读。
#[derive(Debug, Clone, Deserialize)]
pub struct ReasonItem {
    /// 这条规则是否通过。
    #[serde(default)]
    pub complete: Option<bool>,
    /// 服务端的状态码（1 = 通过）。
    #[serde(default)]
    pub complete_status: Option<i64>,
    /// 规则文本，如「按顺序通过所有点位」「跑7分钟以上」。
    #[serde(default)]
    pub reason: Option<String>,
    /// 规则类型。
    #[serde(default, rename = "type")]
    pub kind: Option<i64>,
}

/// 上次提交的判定（记录详情 + 服务端逐条规则）。
#[derive(Debug, Clone)]
pub struct Verdict {
    /// 记录 id。
    pub rrid: i64,
    /// 原文件名。
    pub filename: Option<String>,
    /// 逐条规则判定。
    pub rules: Vec<ReasonItem>,
    /// 服务端给的人话结论。
    pub status_info: Option<String>,
    /// 这条记录用的最小里程（km，selDistance）。
    pub min_distance_km: Option<f64>,
    /// 配速区间（分/km）：(上界, 下界)。
    pub pace_range: Option<(f64, f64)>,
}

impl ReasonItem {
    /// 这条规则过没过。
    pub fn passed(&self) -> bool {
        self.complete.unwrap_or(false) || self.complete_status == Some(1)
    }
}

/// 一条跑步记录的详情（`/api/record/{rrid}`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunRecordDetail {
    pub rrid: i64,
    /// 逐条规则判定。
    #[serde(default)]
    pub reason_list: Option<Vec<ReasonItem>>,
    /// 服务端给的人话结论。
    #[serde(default)]
    pub status_info: Option<String>,
    /// 点位配速（min/km，不是 km/h）。
    #[serde(default)]
    pub speed: Option<f64>,
    /// 规则要求的最小单次里程（km）——服务端这条记录用的门槛。
    #[serde(default)]
    pub sel_distance: Option<f64>,
    /// 配速上界（分/km）。
    #[serde(default)]
    pub speed_top: Option<f64>,
    /// 配速下界（分/km）。
    #[serde(default)]
    pub speed_bottom: Option<f64>,
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
    /// 全校违规通报榜的**人数**（服务端原始响应里的 `list`）。
    ///
    /// ⚠️ 与你自己的违规无关：原始响应里 `self`（你的违规信息，null = 干净）
    /// 和 `list`（全校榜）是两回事，别把这两个数混为一谈。
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

/// `/api/accounts` 的 data：账号列表。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsData {
    /// 当前账号的 id（顶层那个字符串）。
    #[serde(default)]
    pub current: Option<String>,
    /// 账号列表。
    #[serde(default)]
    pub accounts: Vec<AccountSummary>,
}

/// 账号摘要。
///
/// ⚠️ 服务端原始响应里还有姓名与手机号，**这里不建模**（渲染时也不回显）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    /// 账号 id。
    #[serde(default)]
    pub id: Option<String>,
    /// 是否处于登录态。
    #[serde(default)]
    pub logged_in: Option<bool>,
    /// **服务端记的**上次提交时刻（毫秒）。
    ///
    /// 这是服务端的台账：从 Web 控制台交的那次，这里也有。
    /// 插件不自己另记一本（自己记的会漏掉控制台那次提交）。
    #[serde(default)]
    pub last_submit_ms: Option<i64>,
    /// 是否是当前账号。
    #[serde(default)]
    pub current: Option<bool>,
}

/// `/api/starttimes` 的 data：补跑窗口与限制。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTimes {
    /// 当前窗口模式（now = 现在起算）。
    #[serde(default)]
    pub mode: Option<String>,
    /// 窗口起（毫秒）。
    #[serde(default)]
    pub start_ms: Option<i64>,
    /// 窗口止（毫秒）。
    #[serde(default)]
    pub stop_ms: Option<i64>,
    /// 窗口起（可读文本）。
    #[serde(default)]
    pub start_text: Option<String>,
    /// 窗口止（可读文本）。
    #[serde(default)]
    pub stop_text: Option<String>,
    /// 本次时长（秒）。
    #[serde(default)]
    pub duration_sec: Option<i64>,
    /// 服务端是否把这个时段**收窄**过（本来要的点落在未来/越界，被拉回合法窗口）。
    ///
    /// 控制台遇到这个会弹窗让人确认；插件只转述，不自己判断。
    #[serde(default)]
    pub clamped: Option<bool>,
    /// 补跑限制。
    #[serde(default)]
    pub limits: Option<StartLimits>,
}

/// 补跑窗口的限制。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartLimits {
    /// 最多往前补几天。
    #[serde(default)]
    pub max_days_ago: Option<i64>,
    /// 最多往前补多少分钟。
    #[serde(default)]
    pub max_ago_min: Option<i64>,
    /// 随机时刻的下限（点）。
    #[serde(default)]
    pub rand_hour_lo: Option<u64>,
    /// 随机时刻的上限（点）。
    #[serde(default)]
    pub rand_hour_hi: Option<u64>,
}

/// `/api/points` 的 data：必到点（打卡点）。
#[derive(Debug, Clone, Deserialize)]
pub struct PointsData {
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub points: Vec<Checkpoint>,
}

/// 一个打卡点。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    #[serde(default)]
    pub index: Option<i64>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lng: Option<f64>,
    #[serde(default)]
    pub name: Option<String>,
    /// 是否固定打卡点（服务端标记）。
    #[serde(default)]
    pub is_fixed: Option<i64>,
}

/// `/api/fences` 的 data：围栏多边形。
#[derive(Debug, Clone, Deserialize)]
pub struct FencesData {
    #[serde(default)]
    pub count: Option<usize>,
    #[serde(default)]
    pub fences: Vec<Fence>,
}

/// 一个围栏。
#[derive(Debug, Clone, Deserialize)]
pub struct Fence {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    /// 环（BD-09）。
    #[serde(default)]
    pub ring: Vec<(f64, f64)>,
}

/// `POST /api/plan` 与 `POST /api/run` 的 data。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanData {
    /// 流程是否成功（false = 自检没过，服务端没提交）。
    #[serde(default)]
    pub ok: Option<bool>,
    /// 失败原因（自检没过时在这里）。
    #[serde(default)]
    pub error: Option<String>,
    /// 是否连上了服务端（true = 围栏与打卡点是真实数据）。
    #[serde(default)]
    pub online: Option<bool>,
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub duration: Option<f64>,
    /// 实际里程（米）。
    #[serde(default)]
    pub actual_length: Option<f64>,
    #[serde(default)]
    pub steps: Option<i64>,
    /// 平均步频（步/分）。
    #[serde(default)]
    pub avg_cadence: Option<f64>,
    #[serde(default)]
    pub point_count: Option<usize>,
    #[serde(default)]
    pub bucket_count: Option<usize>,
    /// 是否被致命项拦下（true = 不能提交）。
    #[serde(default)]
    pub blocked: Option<bool>,
    /// 自检摘要，如「全部通过」。
    #[serde(default)]
    pub issue_summary: Option<String>,
    /// 明细问题（每条一个）。
    #[serde(default)]
    pub issues: Vec<String>,
    /// 围栏信息。
    #[serde(default)]
    pub fence: Option<PlanFence>,
    /// 打卡点命中情况。
    #[serde(default)]
    pub checkpoints: Vec<PlanCheckpoint>,
    #[serde(default)]
    pub checkpoint_total: Option<usize>,
    #[serde(default)]
    pub checkpoint_passed: Option<usize>,
    /// 实际用的轨迹模式（stadium = 操场绕圈，free = 自由路线）。
    #[serde(default)]
    pub track_mode: Option<String>,
    /// 是不是从自由路线退回了操场。
    #[serde(default)]
    pub stadium_fallback: Option<bool>,
    /// 规则：最小里程（米）、时长限制。
    #[serde(default)]
    pub policy: Option<PlanPolicy>,
    /// 逐步日志（√/× 结论）。
    #[serde(default)]
    pub logs: Vec<String>,
    /// 备注（如「已连服务端：围栏与打卡点为真实数据，未提交」）。
    #[serde(default)]
    pub note: Option<String>,
}

/// 预演里的围栏信息。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFence {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub run_area_id: Option<i64>,
    /// 围栏回退（选了别的围栏）。
    #[serde(default)]
    pub fallback: Option<bool>,
}

/// 预演里的单个打卡点。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanCheckpoint {
    #[serde(default)]
    pub index: Option<i64>,
    #[serde(default)]
    pub name: Option<String>,
    /// 轨迹是否经过了它。
    #[serde(default)]
    pub passed: Option<bool>,
}

/// 提交时用的规则。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPolicy {
    /// 单次最小里程（米）——校园跑的硬门槛。
    #[serde(default)]
    pub min_distance: Option<f64>,
    /// 时长限制。
    #[serde(default)]
    pub valid_time: Option<f64>,
}

/// `POST /api/run` 的 data：提交结果（形状与 Web 控制台一致）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunData {
    /// 是否真的提交了（false = 自检没过，服务端没收）。
    #[serde(default)]
    pub submitted: Option<bool>,
    /// 记录 id（rrid）。
    #[serde(default)]
    pub record_id: Option<i64>,
    /// 上报的里程（米）。
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub steps: Option<i64>,
    /// 平均步频（步/分）。
    #[serde(default)]
    pub cadence: Option<f64>,
    /// 已上传的明细条数。
    #[serde(default)]
    pub obs_keys: Option<i64>,
    /// 流程日志。
    #[serde(default)]
    pub logs: Vec<String>,
}
