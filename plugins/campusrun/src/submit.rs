//! 「帮我跑校园跑 N 圈 / X 公里」的参数换算。
//!
//! 换算口径与 pace-forge 的 Web 控制台一致（`web/index.html` 的 `paceRange` /
//! `autoDur`）：
//! - 时长自动 = 距离(km) × 配速区间(分/km)，取区间中值（可复现）；
//! - 配速区间默认 5.5~6.6 分/km（服务端允许 2.2~10.0）；
//! - 轨迹自动规划（`mode=free`）、经纬度留空 = 用围栏中心。
//!
//! 插件只做**两件**工具不做的事：把用户的话换算成参数、把提交拆成两步确认。
//! 门槛、规则、起跑时刻、能不能过——**一律服务端说了算**，插件只转述：
//! - 距离门槛：服务端 `policy.minDistance`（各门课不一样，不写死）；
//! - 时长下限：上次记录 `reasonList` 里的「跑N分钟以上」；
//! - 起跑时刻：`/api/starttimes`（控制台 `resolveStartAt()` 走的就是它）。

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
    /// 服务端把起跑时段收窄过（转述用）。
    pub start_clamped: bool,
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

/// 换算出来的基础参数（还没定起跑时刻）。
#[derive(Debug, Clone)]
pub struct BaseParams {
    /// 距离（公里）。
    pub distance_km: f64,
    /// 时长（分钟）。
    pub duration_min: f64,
    /// 道次。
    pub lane: u8,
    /// 原始圈数（按公里时为 None）。
    pub laps: Option<f64>,
    /// 海拔区间（米）。
    pub alt_range: Option<(f64, f64)>,
}

/// 起跑时刻（**服务端给的**，不是插件自己算的）。
#[derive(Debug, Clone, Default)]
pub struct StartAt {
    /// 起跑时刻（毫秒）；None = 「刚跑完」（不发 startAtMs）。
    pub at_ms: Option<i64>,
    /// 服务端把这个时段收窄过（本来要的点越界/落在未来，被拉回合法窗口）。
    pub clamped: bool,
}

/// 把意图换算成距离/时长（**不含**起跑时刻）。
///
/// 只做换算，不判门槛：时长/配速/点位全由 pace-forge 的 plan + preflight 判，
/// 插件**不加任何自己的规则**——和 Web 端完全等价。
pub fn base_params(
    section: &CampusrunSection,
    intent: &RunIntent,
    now_ms: i64,
) -> Result<BaseParams> {
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
    // 时长自动：**照抄控制台的 autoDur(dist)**（见 auto_duration_min）。
    let duration_min = auto_duration_min(section, distance_km, now_ms);
    Ok(BaseParams {
        distance_km,
        duration_min,
        lane,
        laps,
        alt_range: section.alt_range(),
    })
}

/// 时长自动（**照抄** pace-forge Web 控制台的 `autoDur(dist)`）。
///
/// ```js
/// function autoDur(dist) {
///   const [lo, hi] = paceRange();               // 配速区间（各自要落在 [2,12]）
///   const pace = lo + Math.random() * (hi - lo);// 区间里**随机**取一个配速
///   return Math.max(3, Math.round(dist * pace * 60) / 60);  // 下限 3 分钟
/// }
/// ```
///
/// 为什么插件也要自己算：服务端的默认时长是**固定 720 秒**（实测 1.2/2.0/3.0 km
/// 全是 12.0 分钟），**不按距离算**——按距离的 autoDur 在控制台的浏览器 JS 里。
/// 但算法必须**一模一样**，不自创变体（之前这里取的是区间中值，控制台是随机的）。
pub fn auto_duration_min(section: &CampusrunSection, distance_km: f64, now_ms: i64) -> f64 {
    let (lo, hi) = section.pace_range();
    let pace = lo + pseudo_random_unit(now_ms) * (hi - lo);
    // 控制台：Math.max(3, Math.round(dist * pace * 60) / 60)
    let minutes = (distance_km * pace * 60.0).round() / 60.0;
    minutes.max(3.0)
}

/// 伪随机数（0.0~1.0）。
///
/// 控制台用 `Math.random()`，插件这边不引随机数依赖，用 splitmix64 的 finalizer
/// 近似——**注意别用 xorshift**：它的低位变化传不到高位，取高 53 位当随机数会
/// 几乎不变（踩过：配速恒定在 6'00"）。
///
/// 种子 = 当前时刻 + 进程内自增计数：同一秒内连着调用也要给出不同的值
/// （`ru41::util::now()` 只到秒，光靠它同一秒内是同一个数）。
fn pseudo_random_unit(seed_source: i64) -> f64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut z = (seed_source as u64)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(counter.wrapping_mul(0xbf58_476d_1ce4_e5b9));
    // splitmix64 finalizer
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// 什么时候跑（**按次请求给**，不来自配置）。
///
/// 谁定：由 **agent 从对话里读出来**再传进来——
/// 「帮我跑」= 用默认；「补一下半小时前那次」= 半小时前；「今天早上九点那次」= 指定时刻。
/// 插件自己不再猜、配置里也不再有「默认什么时候跑」。
#[derive(Debug, Clone, PartialEq)]
pub enum StartIntent {
    /// 刚跑完：不发 startAtMs，服务端按「刚跑完」记。
    ///
    /// 也是 Web 端和工具 CLI（`pace-forge run 2`）的默认档，要它得显式 `--now`。
    Now,
    /// 默认档：**用户给的推荐参数**——当天随机，几天前 0（2026-11-16 建议参数表）。
    Rand { days: i64 },
    /// N 分钟前补跑。
    Ago { minutes: i64 },
    /// 指定时刻补跑（往前 `days` 天）。
    At { hhmm: String, days: i64 },
}

impl Default for StartIntent {
    /// 默认 = 用户给的推荐参数：当天随机，几天前 0。
    fn default() -> Self {
        Self::Rand { days: 0 }
    }
}

/// 起跑方式 → 服务端 `/api/starttimes` 的查询串。
///
/// **起跑时刻归服务端算**：控制台的 `resolveStartAt()` 就是把这些参数丢给
/// `/api/starttimes`，拿回 `startMs` 直接当起跑时刻。插件照抄这一步，
/// 不自己那套「在 [lo,hi] 点里随机挑一个整点」的规则——那是重造轮子，
/// 还会漏掉服务端的收窄（clamped）与越界限制（maxDaysAgo）。
///
/// `None` = 「刚跑完」：不发 startAtMs（跟控制台 `mode === 'now'` 一致）。
pub fn start_query(intent: &StartIntent, duration_sec: i64) -> Result<Option<String>> {
    let dur = duration_sec.max(1);
    let query = match intent {
        StartIntent::Now => return Ok(None),
        StartIntent::Ago { minutes } => format!("mode=ago&minutes={minutes}&dur={dur}"),
        StartIntent::At { hhmm, days } => {
            let (hour, minute) = parse_hhmm(hhmm)?;
            format!(
                "mode=at&hhmm={hour:02}:{minute:02}&days={days}&dur={dur}",
                days = days.max(&0)
            )
        }
        StartIntent::Rand { days } => format!("mode=rand&days={}&dur={dur}", days.max(&0)),
    };
    Ok(Some(query))
}

/// 拼成完整参数。
pub fn plan_params(base: BaseParams, start: StartAt) -> RunParams {
    RunParams {
        distance_km: base.distance_km,
        duration_min: base.duration_min,
        lane: base.lane,
        laps: base.laps,
        start_at_ms: start.at_ms,
        start_clamped: start.clamped,
        alt_range: base.alt_range,
    }
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
    /// 服务端把起跑时段收窄过（转述用）。
    #[serde(default)]
    pub start_clamped: bool,
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
            base_params(
                &section(),
                &RunIntent::Laps { laps: 3.0, lane: 1 },
                1_794_890_000_000,
            )
            .unwrap(),
            StartAt::default(),
        );
        assert!((params.distance_km - 1.2).abs() < 0.01, "{params:?}");
        assert!(
            params.duration_min >= 6.6 && params.duration_min <= 7.9,
            "{params:?}"
        );
        let pace = params.pace_min_per_km();
        assert!((5.5..=6.6).contains(&pace), "配速 {pace}");
    }

    /// 时长照抄控制台的 `autoDur(dist)`：配速在**用户给的区间**里随机取，
    /// 时长 = 距离 × 配速，下限 3 分钟。
    #[test]
    fn auto_duration_copies_the_console_formula() {
        let section = section();
        for seed in 0..50 {
            let now = 1_794_890_000_000 + seed * 37_000;
            let duration = auto_duration_min(&section, 1.2, now);
            assert!(
                (6.6..=7.92).contains(&duration),
                "1.2 km × 配速 5.5~6.6 → 6.6~7.92 分钟，实得 {duration}"
            );
        }
        // 下限 3 分钟：0.4 km × 5.5 = 2.2 分钟 → 抬到 3 分钟（控制台的 max(3, …)）。
        assert_eq!(auto_duration_min(&section, 0.4, 1_794_890_000_000), 3.0);
    }

    /// 随机数要**真的散开**：同一秒内连着调用也得给出不同的配速。
    ///
    /// （踩过的坑：xorshift 的低位变化传不到高位，取高 53 位当随机数 →
    /// 配速恒定在 6'00"，同一个区间里只有「区间中值」一种结果。）
    #[test]
    fn random_pace_actually_varies() {
        let section = section();
        let now = 1_794_890_000_000;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..30 {
            let duration = auto_duration_min(&section, 1.2, now);
            seen.insert((duration * 100.0) as i64);
        }
        assert!(
            seen.len() >= 5,
            "同一时刻连调 30 次只出了 {} 种时长：{seen:?}",
            seen.len()
        );
    }

    /// **用户给的配速原样用**：插件不夹范围、不回落默认。
    #[test]
    fn pace_parameters_are_used_as_given() {
        let mut section = section();
        section.pace_min = Some(4.0);
        section.pace_max = Some(12.0);
        assert_eq!(section.pace_range(), (4.0, 12.0));
        for seed in 0..50 {
            let now = 1_794_890_000_000 + seed * 37_000;
            let duration = auto_duration_min(&section, 1.0, now);
            assert!((4.0..=12.0).contains(&duration), "实得 {duration}");
        }
    }

    /// 换算只管换算：**门槛不在这儿判**（工具的 preflight 判）。
    /// 1 圈 = 400 m；距离不够插件也不拦（Web 端同样不拦）。
    #[test]
    fn conversion_only_never_invents_gates() {
        let one = plan_params(
            base_params(
                &section(),
                &RunIntent::Laps { laps: 1.0, lane: 1 },
                1_794_890_000_000,
            )
            .unwrap(),
            StartAt::default(),
        );
        assert!((one.distance_km - 0.4).abs() < 0.001, "{one:?}");
        assert_eq!(one.laps, Some(1.0));
        let short = plan_params(
            base_params(
                &section(),
                &RunIntent::DistanceKm { km: 0.8 },
                1_794_890_000_000,
            )
            .unwrap(),
            StartAt::default(),
        );
        assert!((short.distance_km - 0.8).abs() < 0.001);
    }

    /// 起跑时刻**交给服务端**：插件只把意图翻成查询串，不自己算随机点。
    #[test]
    fn start_query_defers_to_the_server() {
        let dur = 438;
        let query = start_query(&StartIntent::Rand { days: 2 }, dur).unwrap().unwrap();
        assert_eq!(query, "mode=rand&days=2&dur=438");

        let query = start_query(
            &StartIntent::At {
                hhmm: "07:30".to_owned(),
                days: 0,
            },
            dur,
        )
        .unwrap()
        .unwrap();
        assert_eq!(query, "mode=at&hhmm=07:30&days=0&dur=438");

        let query = start_query(&StartIntent::Ago { minutes: 60 }, dur).unwrap().unwrap();
        assert_eq!(query, "mode=ago&minutes=60&dur=438");
    }

    /// 「刚跑完」不发 startAtMs（跟控制台 `mode === 'now'` 一致），要它得显式 `--now`。
    #[test]
    fn start_intent_now_sends_no_start_at() {
        assert!(start_query(&StartIntent::Now, 438).unwrap().is_none());
    }

    /// 默认档 = 用户给的推荐参数：当天随机，几天前 0。
    #[test]
    fn default_start_intent_is_the_users_recommended_parameter() {
        assert_eq!(StartIntent::default(), StartIntent::Rand { days: 0 });
        let query = start_query(&StartIntent::default(), 438).unwrap().unwrap();
        assert_eq!(query, "mode=rand&days=0&dur=438");
    }

    /// 时刻格式不对要报错，别默默用默认值。
    #[test]
    fn bad_clock_is_rejected() {
        assert!(
            start_query(
                &StartIntent::At {
                    hhmm: "晚上".to_owned(),
                    days: 0
                },
                438
            )
            .is_err()
        );
    }

    #[test]
    fn tokens_differ_per_call() {
        let a = make_token("m1", 100);
        let b = make_token("m1", 200);
        assert_eq!(a.len(), 6);
        assert_ne!(a, b);
    }
}
