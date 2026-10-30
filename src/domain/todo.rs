use anyhow::{Context, Result, ensure};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

/// Todo 记录统一存放在通用 records 中，避免为首版个人待办引入第二套存储。
pub const TODO_KIND: &str = "todo_item";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Active,
    Completed,
    Cancelled,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TodoPriority {
    Low,
    Normal,
    High,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoSource {
    pub channel: String,
    pub source_id: Option<String>,
}

/// 创建待办的宿主输入；截止日期已由调用方解析成 Unix 秒。
#[derive(Clone, Debug)]
pub struct TodoInput {
    pub title: String,
    pub priority: TodoPriority,
    pub labels: Vec<String>,
    pub due_at: Option<i64>,
    pub remind_at: Option<i64>,
    pub source: Option<TodoSource>,
}

#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    pub status: TodoStatus,
    pub priority: TodoPriority,
    #[serde(default)]
    pub labels: Vec<String>,
    /// 截止时间的 Unix 秒；全天日期按本地时区 23:59:59 解释。
    pub due_at: Option<i64>,
    /// 到点提醒时间；常驻提醒循环按该字段发送。
    pub remind_at: Option<i64>,
    /// 最近一次已送达的提醒时间；修改 remind_at 后重新触发。
    #[serde(default)]
    pub last_reminded_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub source: Option<TodoSource>,
}

/// 平台作业写入的 channel 集合；与各平台服务创建 TodoSource 时使用的字面量保持一致。
pub fn is_homework_channel(channel: &str) -> bool {
    matches!(channel, "chaoxing" | "zhihuishu" | "teachermate")
}

impl TodoItem {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty() && self.title.chars().count() <= 500,
            "todo title must contain 1..500 characters"
        );
        ensure!(
            self.labels.len() <= 20,
            "todo item allows at most 20 labels"
        );
        ensure!(
            self.labels.iter().all(|label| !label.trim().is_empty()),
            "todo labels must not be blank"
        );
        ensure!(
            self.created_at >= 0 && self.updated_at >= 0,
            "todo timestamps are invalid"
        );
        if self.status == TodoStatus::Completed {
            ensure!(
                self.completed_at.is_some(),
                "completed todo requires completed_at"
            );
        }
        Ok(())
    }

    pub fn short_id(&self) -> &str {
        self.id.strip_prefix("todo_").unwrap_or(&self.id)
    }

    /// 平台作业条目（超星/智慧树/学习通等）只走各自平台的作业聚合与提醒，
    /// 不混入个人待办视图（/待办、overview、到期提醒）。
    pub fn is_homework(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| is_homework_channel(&source.channel))
    }

    pub fn due_date(&self, options: &TodoOptions) -> Option<String> {
        self.due_at
            .map(|due| local_date(due, options.offset_seconds))
    }
}

/// TodoOptions 是配置边界附带的小值类型，定义在 ru41-plugin-api；
/// 本模块保持原路径，其余 todo 域类型不跨插件边界，留在宿主。
pub use ru41_plugin_api::domain::todo::TodoOptions;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoView {
    Today,
    Recent,
    LongTerm,
    Overdue,
    Inbox,
    All,
}

#[derive(Debug, Default, Serialize, schemars::JsonSchema)]
pub struct TodoOverview {
    pub overdue: Vec<TodoItem>,
    pub today: Vec<TodoItem>,
    pub recent: Vec<TodoItem>,
    /// 未排期事项和超过长期窗口的事项都放这里。
    pub long_term: Vec<TodoItem>,
    pub inbox: Vec<TodoItem>,
}

impl TodoOverview {
    pub fn active_count(&self) -> usize {
        self.overdue.len()
            + self.today.len()
            + self.recent.len()
            + self.long_term.len()
            + self.inbox.len()
    }
}

fn fixed_offset(offset_seconds: i32) -> Result<FixedOffset> {
    FixedOffset::east_opt(offset_seconds).context("invalid todo timezone offset")
}

/// 指定时区中的民用日期（YYYY-MM-DD）。
pub fn local_date(timestamp: i64, offset_seconds: i32) -> String {
    let offset = match fixed_offset(offset_seconds) {
        Ok(offset) => offset,
        Err(_) => return String::new(),
    };
    DateTime::from_timestamp(timestamp, 0)
        .map(|utc| utc.with_timezone(&offset).date_naive().to_string())
        .unwrap_or_default()
}

/// 支持 ISO 日期/日期时间，以及“今天、今晚、明天、明晚、后天、N天后、下午3点”等常用表达。
pub fn parse_due(text: &str, now: i64, options: &TodoOptions) -> Result<i64> {
    let normalized = text.trim().replace('T', " ");
    ensure!(!normalized.is_empty(), "todo due time is required");
    let offset = fixed_offset(options.offset_seconds)?;
    let today = DateTime::from_timestamp(now, 0)
        .context("invalid current timestamp")?
        .with_timezone(&offset)
        .date_naive();

    let naive = parse_iso_datetime(&normalized)
        .or_else(|| parse_relative_datetime(&normalized, today))
        .with_context(|| format!("unsupported todo due time: {text}"))?;
    let local = offset
        .from_local_datetime(&naive)
        .single()
        .context("todo due time is not representable in configured timezone")?;
    Ok(local.timestamp())
}

fn parse_iso_datetime(text: &str) -> Option<NaiveDateTime> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(23, 59, 59))
        .or_else(|| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").ok())
        .or_else(|| NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok())
}

fn parse_relative_datetime(text: &str, today: NaiveDate) -> Option<NaiveDateTime> {
    let mut text = text.trim().replace(' ', "");
    let mut date = today;
    let mut hour: Option<u32> = None;
    let mut minute = 0_u32;
    let mut afternoon = false;

    for (prefix, days) in [
        ("今天", 0_i64),
        ("今日", 0),
        ("明天", 1),
        ("明晚", 1),
        ("后天", 2),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            date = today.checked_add_signed(chrono::Duration::days(days))?;
            text = rest.to_owned();
            break;
        }
    }

    if text == "今晚" {
        text.clear();
        hour = Some(22);
    }
    if let Some(rest) = text.strip_suffix("天后") {
        let days: i64 = rest.parse().ok()?;
        if !(1..=3660).contains(&days) {
            return None;
        }
        date = today.checked_add_signed(chrono::Duration::days(days))?;
        text.clear();
    }

    for marker in ["下午", "晚上", "晚"] {
        if let Some(rest) = text.strip_prefix(marker) {
            afternoon = true;
            text = rest.to_owned();
            break;
        }
    }
    if let Some(rest) = text.strip_prefix("上午") {
        text = rest.to_owned();
    }

    if let Some((h, m)) = parse_clock(&text) {
        hour = Some(h);
        minute = m;
        text.clear();
    }
    if !text.is_empty() {
        return None;
    }

    let mut hour = hour.unwrap_or(22);
    if afternoon && hour < 12 {
        hour += 12;
    }
    date.and_hms_opt(hour, minute, 0)
}

pub(crate) fn parse_clock(text: &str) -> Option<(u32, u32)> {
    let text = text.trim_end_matches('点').trim_end_matches('时');
    let (hour, minute) = if let Some((hour, minute)) = text.split_once(':') {
        (hour.parse().ok()?, minute.parse().ok()?)
    } else if let Some((hour, minute)) = text.split_once('点') {
        (
            hour.parse().ok()?,
            minute.trim_end_matches('分').parse().ok()?,
        )
    } else {
        (text.parse().ok()?, 0)
    };
    ((0..24).contains(&hour) && minute < 60).then_some((hour, minute))
}

/// 待办截止时间的展示标签（本地时区）；`overdue` 为 true 且已过期时附「已过期 N 小时/天」。
/// 展示文案集中在这里，QQ 命令与主动提醒共用一份，避免各处漂移。
pub fn due_label(due_at: Option<i64>, now: i64, options: TodoOptions, overdue: bool) -> String {
    let Some(due_at) = due_at else {
        return "未排期".to_owned();
    };
    let Some(offset) = FixedOffset::east_opt(options.offset_seconds) else {
        return "时间无效".to_owned();
    };
    let mut label = DateTime::from_timestamp(due_at, 0)
        .map(|utc| {
            utc.with_timezone(&offset)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| "时间无效".to_owned());
    if overdue && due_at < now {
        let hours = (now - due_at).div_euclid(3600).max(1);
        label.push_str(&if hours < 24 {
            format!("，已过期 {hours} 小时")
        } else {
            format!("，已过期 {} 天", hours.div_euclid(24))
        });
    }
    label
}
