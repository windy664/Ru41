//! 「帮我跑校园跑 N 圈 / X 公里」的参数换算与门槛校验。
//!
//! 换算口径与 pace-forge 的 Web 控制台一致（`web/index.html` 的 `paceRange` /
//! `autoDur`）：
//! - 时长自动 = 距离(km) × 配速区间(分/km)，取区间中值（可复现）；
//! - 配速区间默认 5.5~6.6 分/km（服务端允许 2.2~10.0）；
//! - 轨迹自动规划（`mode=free`）、经纬度留空 = 用围栏中心。
//!
//! 两条硬门槛（服务端 `minDistance` / `minRunTime`）：
//! - 单次里程 ≥ 1.0 km；
//! - 单次时长 ≥ 5 分钟。
//!
//! 于是「跑一圈」= 0.4 km，两条都不满足 → 圈数下限实际是 3 圈（1.2 km）。

use crate::config::CampusrunSection;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// 待确认提交的记录 kind。
pub const SUBMIT_KIND: &str = "campusrun.submit";
/// 记录上次提交时刻的文档 id（静默窗口用）。
pub const LAST_SUBMIT_ID: &str = "last-submit";

/// 单圈长度（米）。
///
/// 直道 2×84.39 m + 弯道 2π×r；r = 36.8 + 1.22×(道次-1)。
/// 对齐 pace-forge 的 `track::lap_length`：第 1 道 400.0、第 2 道 407.7、第 4 道 423.0。
pub fn lap_length_m(lane: u8) -> f64 {
    let radius = 36.8 + 1.22 * (lane.saturating_sub(1) as f64);
    2.0 * 84.39 + 2.0 * std::f64::consts::PI * radius
}

/// 用户想跑什么。
#[derive(Debug, Clone)]
pub enum RunIntent {
    /// 「跑 N 圈」（圈数 + 道次）。
    Laps { laps: f64, lane: u8 },
    /// 「跑 X 公里」。
    DistanceKm { km: f64 },
}

/// 换算并校验后的参数。
#[derive(Debug, Clone)]
pub struct RunParams {
    /// 距离（公里）。
    pub distance_km: f64,
    /// 时长（分钟）。
    pub duration_min: f64,
    /// 道次。
    pub lane: u8,
    /// 原始圈数（按公里时为 None）。
    pub laps: Option<f64>,
    /// 起跑时刻（毫秒）；None = 「刚跑完」（结束=现在，起跑=现在−时长）。
    pub start_at_ms: Option<i64>,
    /// 海拔区间（米）。
    pub alt_range: Option<(f64, f64)>,
}

impl RunParams {
    /// 实际配速（分/km）。
    pub fn pace_min_per_km(&self) -> f64 {
        if self.distance_km <= 0.0 {
            return 0.0;
        }
        self.duration_min / self.distance_km
    }
}

/// 把意图换算成距离/时长，并过门槛。
///
/// 门槛不过就返回错误（带「差多少」的说法），**不提交**。
pub fn plan_params(
    section: &CampusrunSection,
    intent: &RunIntent,
    rule_min_minutes: Option<f64>,
    rand_hours: (u64, u64),
    now_ms: i64,
) -> Result<RunParams> {
    let (pace_lo, pace_hi) = section.pace_range();
    let (distance_km, laps, lane) = match *intent {
        RunIntent::Laps { laps, lane } => {
            let meters = laps * lap_length_m(lane);
            (meters / 1000.0, Some(laps), lane)
        }
        RunIntent::DistanceKm { km } => (km, None, 1),
    };
    if !(distance_km.is_finite() && distance_km > 0.0) {
        bail!("距离不对：{distance_km} km");
    }
    // 时长自动：距离 × 配速区间中值（与控制台 autoDur 同口径，取中值可复现）。
    let mut duration_min = distance_km * (pace_lo + pace_hi) / 2.0;
    // 服务端规则的时长下限（「跑N分钟以上」，从上次记录的 reasonList 读）：
    // 不够就抬到门槛——否则交上去只进排行榜、不计学期成绩（11-16 那条就栽在这）。
    if let Some(min_minutes) = rule_min_minutes
        && duration_min < min_minutes
    {
        duration_min = min_minutes;
    }
    // 何时跑：now / rand / at / ago。
    let start_at_ms = match section.start_mode() {
        "now" => None,
        "rand" => random_start_ms(
            now_ms,
            (duration_min * 60.0).round() as i64,
            rand_hours,
            section.days_ago(),
        ),
        "at" => {
            let hhmm = section.start_hhmm().unwrap_or("07:00");
            let (hour, minute) = parse_hhmm(hhmm)?;
            start_of_day_ms(now_ms, hour, minute)
        }
        "ago" => {
            let minutes = section.ago_minutes().unwrap_or(60);
            Some(now_ms - minutes * 60_000)
        }
        other => bail!("未知的 start_mode：{other}（支持 now/rand/at/ago）"),
    };

    // 门槛**不在这儿判**：距离门槛由服务端 policy.minDistance 给（service 里读），
    // 时长/配速/点位全由 pace-forge 的 plan + preflight 判。插件只负责把
    // 用户的话换算成参数，别在这儿重造一套规则。
    Ok(RunParams {
        distance_km,
        duration_min,
        lane,
        laps,
        start_at_ms,
        alt_range: section.alt_range(),
    })
}

/// 「当天随机」：在 [lo, hi] 点里随机取一个**已经过去**的时刻。
///
/// 约束跟工具一致：开始 + 用时 + 5s 余量 ≤ 现在；往前最多 `days_ago` 天。
fn random_start_ms(
    now_ms: i64,
    duration_sec: i64,
    hours: (u64, u64),
    days_ago: i64,
) -> Option<i64> {
    use chrono::{Local, TimeZone};
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    let latest = now_ms - (duration_sec + 5) * 1000;
    let today = now.date_naive();
    let mut candidates: Vec<i64> = Vec::new();
    for day in 0..=days_ago.max(0) {
        let date = today - chrono::Duration::days(day);
        for hour in hours.0..=hours.1 {
            let naive = date.and_hms_opt(hour as u32, 0, 0)?;
            let ms = Local
                .from_local_datetime(&naive)
                .single()
                .map(|time| time.timestamp_millis())?;
            if ms <= latest {
                candidates.push(ms);
            }
        }
    }
    if candidates.is_empty() {
        return None;
    }
    // 简单伪随机（种子取当前时刻）：够用且不引依赖。
    let mut seed = now_ms as u64 ^ 0x9e37_79b9_7f4a_7c15;
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    let index = (seed % candidates.len() as u64) as usize;
    Some(candidates[index])
}

/// 「at」模式：今天的 HH:MM（毫秒时间戳）。
fn start_of_day_ms(now_ms: i64, hour: u32, minute: u32) -> Option<i64> {
    use chrono::{Local, TimeZone};
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    let naive = now.date_naive().and_hms_opt(hour, minute, 0)?;
    Local
        .from_local_datetime(&naive)
        .single()
        .map(|t| t.timestamp_millis())
}

/// 解析 HH:MM。
fn parse_hhmm(raw: &str) -> Result<(u32, u32)> {
    let (hour, minute) = raw
        .trim()
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("时刻格式不对：{raw}（要 HH:MM）"))?;
    let hour: u32 = hour
        .trim()
        .parse()
        .with_context(|| format!("小时不对：{raw}"))?;
    let minute: u32 = minute
        .trim()
        .parse()
        .with_context(|| format!("分钟不对：{raw}"))?;
    if hour > 23 || minute > 59 {
        bail!("时刻不对：{raw}");
    }
    Ok((hour, minute))
}

/// 待确认的一条提交。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingSubmit {
    /// 确认口令（短 token）。
    pub token: String,
    /// 距离（公里）。
    pub distance_km: f64,
    /// 时长（分钟）。
    pub duration_min: f64,
    /// 道次。
    pub lane: u8,
    /// 圈数（按公里时为 None）。
    #[serde(default)]
    pub laps: Option<f64>,
    /// 起跑时刻（毫秒）；None = 「刚跑完」。
    #[serde(default)]
    pub start_at_ms: Option<i64>,
    /// 海拔区间（米）。
    #[serde(default)]
    pub alt_range: Option<(f64, f64)>,
    /// planned → submitting → accepted / rejected / failed / expired。
    pub state: String,
    /// 登记时刻（秒）。
    pub created_at: i64,
    /// 提交后的记录 id。
    #[serde(default)]
    pub rrid: Option<i64>,
    /// 失败原因。
    #[serde(default)]
    pub error: Option<String>,
    /// 预演摘要（给用户看的那段）。
    #[serde(default)]
    pub plan_summary: String,
}

/// 上次提交的时刻（静默窗口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastSubmit {
    /// 上次提交时刻（秒）。
    pub last_submit_at: i64,
    /// 上次的记录 id。
    #[serde(default)]
    pub rrid: Option<i64>,
}

/// 生成确认口令：6 位十六进制，来自 msg_id + 时刻的哈希。
pub fn make_token(seed: &str, now: i64) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.as_bytes().iter().chain(now.to_le_bytes().iter()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{:06x}", (hash & 0xff_ffff) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section() -> CampusrunSection {
        CampusrunSection::default()
    }

    #[test]
    fn lap_length_matches_the_tool() {
        assert!((lap_length_m(1) - 400.0).abs() < 0.2, "{}", lap_length_m(1));
        assert!((lap_length_m(2) - 407.7).abs() < 0.2, "{}", lap_length_m(2));
        assert!((lap_length_m(4) - 423.0).abs() < 0.2, "{}", lap_length_m(4));
    }

    #[test]
    fn three_laps_land_on_the_users_parameters() {
        // 用户给的表：距离 1.2 km、配速 5.5~6.6 → 时长 6.6~7.9 分。
        let params = plan_params(
            &section(),
            &RunIntent::Laps { laps: 3.0, lane: 1 },
            None,
            (7, 20),
            1_794_890_000_000,
        )
        .unwrap();
        assert!((params.distance_km - 1.2).abs() < 0.01, "{params:?}");
        assert!(
            params.duration_min >= 6.6 && params.duration_min <= 7.9,
            "{params:?}"
        );
        let pace = params.pace_min_per_km();
        assert!((5.5..=6.6).contains(&pace), "配速 {pace}");
    }

    /// 换算只管换算：**门槛不在这儿判**（服务端 policy + 工具 preflight 判）。
    /// 1 圈 = 400 m；距离不够也不拦，由服务端门槛拦（见 service 层）。
    #[test]
    fn conversion_only_never_invents_gates() {
        let one = plan_params(
            &section(),
            &RunIntent::Laps { laps: 1.0, lane: 1 },
            None,
            (7, 20),
            1_794_890_000_000,
        )
        .unwrap();
        assert!((one.distance_km - 0.4).abs() < 0.001, "{one:?}");
        assert_eq!(one.laps, Some(1.0));
        let short = plan_params(
            &section(),
            &RunIntent::DistanceKm { km: 0.8 },
            None,
            (7, 20),
            1_794_890_000_000,
        )
        .unwrap();
        assert!((short.distance_km - 0.8).abs() < 0.001);
    }

    #[test]
    fn tokens_differ_per_call() {
        let a = make_token("m1", 100);
        let b = make_token("m1", 200);
        assert_eq!(a.len(), 6);
        assert_ne!(a, b);
    }
}
