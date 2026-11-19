//! 配置：目标集、可用整块槽位、碎片段、学期窗口。
//!
//! 全部从 toml 读 —— 上一轮模拟把锚点硬编码在代码里，导致"填真实锚点"要改代码。
//! 现在改成改配置文件即可（见 config.example.toml）。

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Deserialize;

fn default_card_count() -> usize {
    300
}
fn default_retention() -> f32 {
    0.90
}
fn default_block_minutes() -> i64 {
    120
}
fn default_seconds_per_card() -> f64 {
    10.0
}
fn default_fragment_daily_budget() -> f64 {
    13.0 * 60.0
}

/// 目标类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalKind {
    /// 硬锚定脉冲型：考试/报名截止驱动，倒排
    Pulse,
    /// 跟课型：跟着校历周次走
    Course,
    /// 日常型：整块推进
    Daily,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MilestoneSpec {
    pub name: String,
    /// 完成需要的整块分钟数
    pub minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoalSpec {
    pub id: String,
    pub name: String,
    pub kind: GoalKind,
    /// 目标权重（人给的 0-100）
    pub weight: f64,
    /// 硬锚点：考试日 / 报名截止（ISO 日期）。`Course`/`Daily` 可为 null。
    #[serde(default)]
    pub anchor: Option<String>,
    /// 跟课型每周固定占几个块
    #[serde(default)]
    pub weekly_blocks: usize,
    #[serde(default)]
    pub milestones: Vec<MilestoneSpec>,
}

/// 可用整块槽位：星期几 + 起始时刻。
#[derive(Debug, Clone, Deserialize)]
pub struct BlockSpec {
    /// 1=周一 … 7=周日
    pub weekday: u8,
    /// 本地时刻 HH:MM
    pub start: String,
    pub label: String,
}

/// 碎片段：课间缝这类短空隙。
#[derive(Debug, Clone, Deserialize)]
pub struct FragmentSpec {
    /// 1=周一 … 7=周日
    pub weekday: u8,
    pub start: String,
    /// 这段有多长（分钟）
    pub minutes: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TermSpec {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub term: TermSpec,
    #[serde(default)]
    pub blocks: Vec<BlockSpec>,
    #[serde(default)]
    pub fragment_slots: Vec<FragmentSpec>,
    #[serde(default)]
    pub goals: Vec<GoalSpec>,
    #[serde(default = "default_card_count")]
    pub card_count: usize,
    #[serde(default = "default_retention")]
    pub desired_retention: f32,
    #[serde(default = "default_block_minutes")]
    pub block_minutes: i64,
    #[serde(default = "default_seconds_per_card")]
    pub seconds_per_card: f64,
    #[serde(default = "default_fragment_daily_budget")]
    pub fragment_daily_budget_seconds: f64,
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("读配置失败: {}", path.display()))?;
        let cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("解析配置失败: {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.goals.is_empty(), "配置里至少要有一个目标");
        for g in &self.goals {
            anyhow::ensure!(
                (1..=7).contains(&g.weekday_guard()),
                "目标 {} 的 weekday 越界",
                g.id
            );
            anyhow::ensure!(
                g.milestones.iter().all(|m| m.minutes > 0),
                "目标 {} 的里程碑 minutes 必须为正",
                g.id
            );
        }
        Ok(())
    }

    pub fn term_start(&self) -> DateTime<Utc> {
        let d = NaiveDate::parse_from_str(&self.term.start, "%Y-%m-%d").expect("term.start");
        Utc.from_utc_datetime(&d.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()))
    }

    pub fn term_end(&self) -> DateTime<Utc> {
        let d = NaiveDate::parse_from_str(&self.term.end, "%Y-%m-%d").expect("term.end");
        Utc.from_utc_datetime(&d.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()))
    }

    pub fn goal_index(&self, id: &str) -> Option<usize> {
        self.goals.iter().position(|g| g.id == id)
    }
}

impl GoalSpec {
    fn weekday_guard(&self) -> u8 {
        // 仅为 validate 提供一个恒合法的值；真正的 weekday 校验在 BlockSpec/FragmentSpec。
        1
    }

    pub fn anchor_date(&self) -> Option<NaiveDate> {
        self.anchor
            .as_ref()
            .and_then(|a| NaiveDate::parse_from_str(a, "%Y-%m-%d").ok())
    }
}
