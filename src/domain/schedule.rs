//! 行程（日程事件）：时间轴上的占用，与待办（任务闭环）在概念上分离。
//! 事件统一存放在通用 records 中（kind = "event"），周期事件在读取时展开成
//! 具体发生（occurrence），不把展开的实例写进存储。
//!
//! 覆盖用户场景：课表/社团（每周重复）、开会/约会（一次性）、旅游（多日
//! 连续事件，用起止时间直接表达）。占用与冲突检测服务于 agent 规划。

use anyhow::{Result, ensure};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

use super::todo::{TodoOptions, parse_clock};

pub const EVENT_KIND: &str = "event";

/// 事件开始前多少秒触发提醒；与 todo-remind 循环的轮询间隔配合。
pub const REMIND_LEAD_SECS: i64 = 15 * 60;

/// 单事件时长上限（3 天）：跨天行程用起止时间表达，超上限视为误输入。
const MAX_DURATION_SECS: i64 = 3 * 24 * 3600;

/// 已提醒发生时刻的保留上限，防周期事件无限增长。
const REMINDED_STARTS_CAP: usize = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventRecurrence {
    /// 一次性事件。
    None,
    /// 每周重复；weekday_mask 第 0..7 位对应周一..周日，
    /// 发生时刻取 starts_at 的本地时刻，时长取 ends_at - starts_at。
    Weekly {
        #[serde(default)]
        weekday_mask: u8,
    },
}

impl EventRecurrence {
    pub fn is_recurring(&self) -> bool {
        matches!(self, Self::Weekly { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventStatus {
    Active,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventSource {
    pub channel: String,
    pub source_id: Option<String>,
}

/// 创建事件的宿主输入；起止时间已由调用方解析成 Unix 秒。
#[derive(Clone, Debug)]
pub struct EventInput {
    pub title: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub location: Option<String>,
    pub recurrence: EventRecurrence,
    pub source: Option<EventSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventItem {
    pub id: String,
    pub title: String,
    pub status: EventStatus,
    /// 基准发生时刻；Weekly 的本地时刻与起始日由此推算。
    pub starts_at: i64,
    /// 基准结束时刻；时长 = ends_at - starts_at。
    pub ends_at: i64,
    pub location: Option<String>,
    pub recurrence: EventRecurrence,
    /// 已提醒过的发生时刻（升序，截断保留最近若干），重启后不重复提醒。
    #[serde(default)]
    pub reminded_starts: Vec<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub source: Option<EventSource>,
}

/// 一次具体发生：周期事件在窗口内展开的实例，带展示所需的扁平字段。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EventOccurrence {
    pub event_id: String,
    pub title: String,
    pub location: Option<String>,
    pub starts_at: i64,
    pub ends_at: i64,
    pub recurring: bool,
}

impl EventItem {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty() && self.title.chars().count() <= 500,
            "event title must contain 1..500 characters"
        );
        ensure!(
            self.starts_at >= 0 && self.ends_at > self.starts_at,
            "event requires 0 <= starts_at < ends_at"
        );
        ensure!(
            self.ends_at - self.starts_at <= MAX_DURATION_SECS,
            "event duration exceeds 3 days; model multi-day trips as separate events"
        );
        if let EventRecurrence::Weekly { weekday_mask } = self.recurrence {
            ensure!(weekday_mask != 0, "weekly event requires a weekday mask");
        }
        ensure!(
            self.created_at >= 0 && self.updated_at >= 0,
            "event timestamps are invalid"
        );
        Ok(())
    }

    pub fn short_id(&self) -> &str {
        self.id.strip_prefix("evt_").unwrap_or(&self.id)
    }

    pub fn duration_secs(&self) -> i64 {
        (self.ends_at - self.starts_at).max(0)
    }

    pub fn reminded(&self, starts_at: i64) -> bool {
        self.reminded_starts.contains(&starts_at)
    }

    /// 记录一次已提醒的发生；调用方在通知发送成功后才调用。
    pub fn mark_reminded(&mut self, starts_at: i64) {
        if !self.reminded_starts.contains(&starts_at) {
            self.reminded_starts.push(starts_at);
            self.reminded_starts.sort_unstable();
            if self.reminded_starts.len() > REMINDED_STARTS_CAP {
                let drop = self.reminded_starts.len() - REMINDED_STARTS_CAP;
                self.reminded_starts.drain(..drop);
            }
        }
    }

    /// 展开 [window_start, window_end) 内的发生。weekday 按本地时区（固定偏移）推算，
    /// 早于基准日本地的发生被裁掉，避免给周期事件生成创建前的过去实例。
    pub fn occurrences_between(
        &self,
        window_start: i64,
        window_end: i64,
        offset_seconds: i32,
    ) -> Vec<EventOccurrence> {
        let duration = self.duration_secs();
        let make = |starts_at: i64| EventOccurrence {
            event_id: self.id.clone(),
            title: self.title.clone(),
            location: self.location.clone(),
            starts_at,
            ends_at: starts_at + duration,
            recurring: self.recurrence.is_recurring(),
        };
        match self.recurrence {
            EventRecurrence::None => {
                let starts = self.starts_at;
                if starts < window_end && starts + duration > window_start {
                    vec![make(starts)]
                } else {
                    Vec::new()
                }
            }
            EventRecurrence::Weekly { weekday_mask } => {
                let Some(offset) = FixedOffset::east_opt(offset_seconds) else {
                    return Vec::new();
                };
                let Some(base) = DateTime::from_timestamp(self.starts_at, 0) else {
                    return Vec::new();
                };
                let base_local = base.with_timezone(&offset);
                let time_of_day = base_local.time();
                // 基准日本地零点：周期发生的下限。
                let base_day_start = match base_local.date_naive().and_hms_opt(0, 0, 0) {
                    Some(naive) => offset
                        .from_local_datetime(&naive)
                        .single()
                        .map(|dt| dt.timestamp())
                        .unwrap_or(self.starts_at),
                    None => self.starts_at,
                };
                let mut out = Vec::new();
                // 从窗口覆盖的首个本地日起逐日扫描；个人事件量级下逐日展开足够快。
                let first_day = (window_start + offset_seconds as i64)
                    .checked_div_euclid(86400)
                    .unwrap_or(0)
                    * 86400
                    - offset_seconds as i64;
                let mut cursor = first_day;
                while cursor < window_end {
                    if let Some(date) = local_date(cursor, offset_seconds) {
                        let bit = 1u8 << (date.weekday().number_from_monday() - 1);
                        if bit & weekday_mask != 0
                            && let Some(start_naive) = date.and_hms_opt(
                                time_of_day.hour(),
                                time_of_day.minute(),
                                time_of_day.second(),
                            )
                            && let Some(start) = offset
                                .from_local_datetime(&start_naive)
                                .single()
                                .map(|dt| dt.timestamp())
                            && start >= base_day_start
                            && start < window_end
                            && start + duration > window_start
                        {
                            out.push(make(start));
                        }
                    }
                    let Some(next) = cursor.checked_add(86400) else {
                        break;
                    };
                    cursor = next;
                }
                out
            }
        }
    }
}

fn local_date(timestamp: i64, offset_seconds: i32) -> Option<NaiveDate> {
    let offset = FixedOffset::east_opt(offset_seconds)?;
    DateTime::from_timestamp(timestamp, 0).map(|utc| utc.with_timezone(&offset).date_naive())
}

/// 事件时间的展示标签（本地时区）；周期事件带「每X」前缀。
pub fn event_time_label(starts_at: i64, recurring: bool, offset_seconds: i32) -> String {
    let Some(offset) = FixedOffset::east_opt(offset_seconds) else {
        return "时间无效".to_owned();
    };
    let Some(utc) = DateTime::from_timestamp(starts_at, 0) else {
        return "时间无效".to_owned();
    };
    let local = utc.with_timezone(&offset);
    let weekdays = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    let weekday = weekdays[(local.weekday().number_from_monday() - 1) as usize];
    if recurring {
        format!("每{weekday} {}", local.format("%H:%M"))
    } else {
        local.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// 解析自然语言事件时间：`每周二14:00-15:40`、`每周三五六19:00-21:00`、
/// `明天14:00-16:00`、`今晚20:00-22:00`、`10-25 9:00-10:30`。
/// 缺省结束时刻默认 1 小时；结束早于开始视为跨天（+24h）。返回 None 表示
/// 不拦截，消息交回自然语言路径。
pub fn parse_event_when(
    text: &str,
    now: i64,
    options: &TodoOptions,
) -> Option<(i64, i64, EventRecurrence)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(rest) = text.strip_prefix("每周") {
        return parse_weekly_when(rest, now, options);
    }
    let (date, default_hour, clock_text) = split_oneoff(text, now, options)?;
    let (start_clock, end_clock) = if clock_text.is_empty() {
        let hour = default_hour.unwrap_or(20);
        ((hour, 0), None)
    } else {
        parse_clock_range(clock_text)?
    };
    let offset = FixedOffset::east_opt(options.offset_seconds)?;
    let start_naive = date.and_hms_opt(start_clock.0, start_clock.1, 0)?;
    let starts = offset
        .from_local_datetime(&start_naive)
        .single()?
        .timestamp();
    let ends = end_timestamp(start_naive, end_clock, &offset)?;
    Some((starts, ends, EventRecurrence::None))
}

/// 每周X[Y...] <clock-range>：基准日取今天（当时刻未过）或之后最近的匹配日。
fn parse_weekly_when(
    rest: &str,
    now: i64,
    options: &TodoOptions,
) -> Option<(i64, i64, EventRecurrence)> {
    let digit_pos = rest.find(|c: char| c.is_ascii_digit())?;
    let (weekday_part, clock_part) = rest.split_at(digit_pos);
    let mask = weekday_mask(weekday_part.trim_end_matches([' ', '的']))?;
    let (start_clock, end_clock) = parse_clock_range(clock_part.trim_start_matches([' ', '的']))?;
    let offset = FixedOffset::east_opt(options.offset_seconds)?;
    let today = local_date(now, options.offset_seconds)?;
    let today_bit = 1u8 << (today.weekday().number_from_monday() - 1);
    let start_today = offset
        .from_local_datetime(&today.and_hms_opt(start_clock.0, start_clock.1, 0)?)
        .single()?
        .timestamp();
    let base_day = if mask & today_bit != 0 && now < start_today {
        today
    } else {
        (1..=7)
            .map(|d| today.checked_add_signed(Duration::days(d)))
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .find(|d| mask & (1u8 << (d.weekday().number_from_monday() - 1)) != 0)?
    };
    let start_naive = base_day.and_hms_opt(start_clock.0, start_clock.1, 0)?;
    let starts = offset
        .from_local_datetime(&start_naive)
        .single()?
        .timestamp();
    let ends = end_timestamp(start_naive, end_clock, &offset)?;
    Some((starts, ends, EventRecurrence::Weekly { weekday_mask: mask }))
}

/// 「二」「三五」「六日」「天」→ weekday_mask（周一 = bit0）。
fn weekday_mask(text: &str) -> Option<u8> {
    let names = ['一', '二', '三', '四', '五', '六', '日'];
    let mut mask = 0u8;
    for c in text.chars() {
        let idx = if c == '天' {
            6
        } else {
            names.iter().position(|n| *n == c)?
        };
        mask |= 1 << idx;
    }
    (mask != 0).then_some(mask)
}

/// 一次性事件：切出日期前缀与剩余时刻文本。支持 今天/明天/后天/今晚/
/// M-D / M月D日 / YYYY-M-D；无前缀视为今天。月-日早于今天时滚到下一年。
fn split_oneoff<'a>(
    text: &'a str,
    now: i64,
    options: &TodoOptions,
) -> Option<(NaiveDate, Option<u32>, &'a str)> {
    let today = local_date(now, options.offset_seconds)?;
    for (prefix, days) in [("今天", 0i64), ("今日", 0), ("明天", 1), ("后天", 2)] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let date = today.checked_add_signed(Duration::days(days))?;
            return Some((date, None, rest.trim_start_matches([' ', '的'])));
        }
    }
    if let Some(rest) = text.strip_prefix("今晚") {
        return Some((today, Some(20), rest.trim_start_matches([' ', '的'])));
    }
    let head_end = text
        .find(|c: char| c.is_ascii_digit() || c.is_whitespace())
        .unwrap_or(text.len());
    let head = &text[..head_end];
    if head.contains('月') || head.contains('-') || head.chars().all(|c| c.is_ascii_digit()) {
        let digits: String = head
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        if let Some(mut date) = parse_month_day(&digits, today) {
            if date < today {
                date = date.checked_add_signed(Duration::days(365))?;
            }
            let rest = text[head.len()..].trim_start_matches([' ', '的']);
            return Some((date, None, rest));
        }
    }
    Some((today, None, text))
}

/// M-D / M月D日（head 已去掉「月」「日」等字，只留数字与连字符）→ 日期。
fn parse_month_day(digits: &str, today: NaiveDate) -> Option<NaiveDate> {
    let parts: Vec<&str> = digits.split('-').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [month, day] => {
            NaiveDate::from_ymd_opt(today.year(), month.parse().ok()?, day.parse().ok()?)
        }
        [year, month, day] => {
            NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
        }
        _ => None,
    }
}

/// 本地时刻（时, 分）。
type Clock = (u32, u32);

/// 解析时刻范围 `14:00-16:00` / `9点-10点半` / `20:00`；缺省结束 = 开始 + 1h。
fn parse_clock_range(text: &str) -> Option<(Clock, Option<Clock>)> {
    let text = text.trim();
    for sep in ['-', '–', '—', '~', '到'] {
        if let Some((a, b)) = text.split_once(sep)
            && let (Some(start), Some(end)) = (parse_clock(a.trim()), parse_clock(b.trim()))
        {
            return Some((start, Some(end)));
        }
    }
    parse_clock(text).map(|start| (start, None))
}

/// 结束时间戳：缺省 +1h；结束不晚于开始时视为跨天（+24h）。
fn end_timestamp(
    start: chrono::NaiveDateTime,
    end: Option<Clock>,
    offset: &FixedOffset,
) -> Option<i64> {
    let end_naive = match end {
        None => return Some(start.and_utc().timestamp() + 3600),
        Some((h, m)) => {
            let naive = start.date().and_hms_opt(h, m, 0)?;
            if naive <= start {
                naive + Duration::days(1)
            } else {
                naive
            }
        }
    };
    offset
        .from_local_datetime(&end_naive)
        .single()
        .map(|dt| dt.timestamp())
}
