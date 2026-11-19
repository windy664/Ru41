//! 规划核心：纯函数，不碰 IO。
//!
//! ## 分工（2026-11-20 修正）
//!
//! - **LLM 负责把一件事拆成带时长的子任务流**（"看定义 40min / 做例题 60min / 自测 20min"）
//! - **算法只负责装箱**：把 step 流装进时间槽，允许跨块切分
//!
//! 上一版把里程碑当成"整块扣减"的原子单位（`remaining -= 120` 再 `.max(0)`），
//! 会**吞掉时间**：某个里程碑还差 90 分钟、块是 120 分钟 → 直接归零，30 分钟凭空消失。
//! 现在 step 可以跨块延续：周一晚装 40min 的"看定义" + 20min 的"做例题"，
//! 剩下的 40min"做例题"接着排到周日上午。
//!
//! 分配顺序固定为 **硬锚定 → 软基础量 → 跟课 → 剩余自由**。
//! 硬约束必须先于弹性项，否则弹性项会把可用块吃光、把硬锚定目标饿死
//! （2026-11-19 模拟实测 bug 1，已固化成回归测试）。

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use std::collections::VecDeque;

use crate::config::{GoalKind, GoalSpec};

/// 一次整块分配的结果类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// 硬锚定（锚点在本学期内，必须完成）
    Hard,
    /// 基础量（锚点在本学期外，本学期只打基础）
    Base,
    /// 跟课绑定
    Course,
    /// 剩余整块的自由分配
    Flexible,
}

impl BlockKind {
    pub fn label(&self) -> &'static str {
        match self {
            BlockKind::Hard => "硬锚定",
            BlockKind::Base => "基础量",
            BlockKind::Course => "跟课",
            BlockKind::Flexible => "日常",
        }
    }
}

/// 装箱结果的一段：**一个 step 在某个块里占用的一段时间**。
///
/// 一个 step 可能被切成多段（跨块），每段一条记录。
#[derive(Debug, Clone)]
pub struct AllocatedSegment {
    pub date: DateTime<Utc>,
    pub label: String,
    pub goal_name: String,
    pub milestone_name: String,
    pub step_name: String,
    /// 从块开始算的偏移（分钟）
    pub offset_minutes: i64,
    /// 本段占用时长（分钟）
    pub duration_minutes: i64,
    pub kind: BlockKind,
}

/// 一个可用整块槽位（具体到某天）。
#[derive(Debug, Clone)]
pub struct Slot {
    pub date: DateTime<Utc>,
    pub label: String,
}

/// 子任务状态（LLM 拆解出来的一个步骤）。
#[derive(Debug, Clone)]
pub struct StepState {
    pub name: String,
    /// 还剩多少分钟（跨块扣减，不丢时间）
    pub remaining_minutes: i64,
}

/// 里程碑状态：它下面的一串子任务。
#[derive(Debug, Clone)]
pub struct MilestoneState {
    pub name: String,
    pub steps: Vec<StepState>,
    /// 当前做到第几个 step
    pub cursor: usize,
}

/// 目标状态：里程碑 + 子任务流 + 已用块数。
#[derive(Debug, Clone)]
pub struct GoalState {
    pub spec: GoalSpec,
    pub milestones: Vec<MilestoneState>,
    pub blocks_used: u64,
}

impl GoalState {
    pub fn new(spec: GoalSpec) -> Self {
        let milestones = spec
            .milestones
            .iter()
            .map(|m| MilestoneState {
                name: m.name.clone(),
                steps: m
                    .steps
                    .iter()
                    .map(|s| StepState {
                        name: s.name.clone(),
                        remaining_minutes: s.minutes,
                    })
                    .collect(),
                cursor: 0,
            })
            .collect();
        Self {
            spec,
            milestones,
            blocks_used: 0,
        }
    }

    /// 剩余总分钟数（所有未完成 step）
    pub fn remaining_minutes(&self) -> i64 {
        self.milestones
            .iter()
            .flat_map(|m| m.steps.iter().map(|s| s.remaining_minutes))
            .sum()
    }

    pub fn done(&self) -> bool {
        self.remaining_minutes() <= 0
    }

    /// 当前应做的 step（第一个未完成里程碑的队首未完成 step）
    fn current_step(&self) -> Option<(usize, usize)> {
        for (mi, m) in self.milestones.iter().enumerate() {
            if let Some(si) = m.steps[m.cursor..]
                .iter()
                .position(|s| s.remaining_minutes > 0)
            {
                return Some((mi, m.cursor + si));
            }
        }
        None
    }
}

/// 本周某个目标的需求。
#[derive(Debug, Clone, Copy)]
pub struct Demand {
    pub goal_idx: usize,
    /// 本周至少要几个块
    pub blocks: usize,
    /// 距离锚点还有几周
    pub weeks_left: i64,
    /// 锚点是否在本学期内（true=硬需求，false=只做基础量）
    pub hard: bool,
}

/// 算出本周各硬锚定目标的需求。
pub fn compute_demands(
    goals: &[GoalState],
    week_start: DateTime<Utc>,
    term_end: DateTime<Utc>,
    block_minutes: i64,
    base_blocks: usize,
) -> (Vec<Demand>, Vec<Demand>) {
    let mut hard = Vec::new();
    let mut soft = Vec::new();
    for (i, st) in goals.iter().enumerate() {
        if st.spec.kind != GoalKind::Pulse {
            continue;
        }
        let Some(anchor) = st.spec.anchor_date() else {
            continue;
        };
        let anchor_dt = anchor
            .and_hms_opt(0, 0, 0)
            .map(|n| Utc.from_utc_datetime(&n))
            .expect("anchor");
        let weeks_left = ((anchor_dt - week_start).num_days() as f64 / 7.0).ceil() as i64;
        if weeks_left <= 0 {
            continue;
        }
        if anchor_dt <= term_end {
            hard.push(Demand {
                goal_idx: i,
                blocks: required_blocks_per_week(st, weeks_left, block_minutes),
                weeks_left,
                hard: true,
            });
        } else {
            soft.push(Demand {
                goal_idx: i,
                blocks: base_blocks,
                weeks_left,
                hard: false,
            });
        }
    }
    (hard, soft)
}

/// 倒排：剩余时间必须在这学期完成 → 每周需要几个块。
pub fn required_blocks_per_week(st: &GoalState, weeks_left: i64, block_minutes: i64) -> usize {
    let remaining = st.remaining_minutes();
    if remaining <= 0 {
        return 0;
    }
    let weeks = weeks_left.max(1);
    let per_week = (remaining + weeks - 1) / weeks;
    ((per_week + block_minutes - 1) / block_minutes).max(1) as usize
}

/// 跟课型每周固定占几个块。
pub fn course_demand(goals: &[GoalState]) -> usize {
    goals
        .iter()
        .filter(|st| st.spec.kind == GoalKind::Course)
        .map(|st| st.spec.weekly_blocks)
        .sum()
}

/// **从目标里取 step 填满一个块，跨块切分。**
///
/// `budget` 是这个块剩余的分钟数（会被消耗）。
/// step 的剩余部分保留在目标里，下一个块接着做 —— **不丢时间**。
fn consume_into_block(
    st: &mut GoalState,
    kind: BlockKind,
    slot: &Slot,
    block_minutes: i64,
    budget: &mut i64,
    out: &mut Vec<AllocatedSegment>,
) {
    let blocks_before = st.blocks_used;
    let mut consumed_any = false;
    while *budget > 0 {
        let Some((mi, si)) = st.current_step() else {
            break;
        };
        let milestone_name = st.milestones[mi].name.clone();
        let step = &mut st.milestones[mi].steps[si];
        let step_name = step.name.clone();
        let take = step.remaining_minutes.min(*budget);
        out.push(AllocatedSegment {
            date: slot.date,
            label: slot.label.clone(),
            goal_name: st.spec.name.clone(),
            milestone_name,
            step_name,
            offset_minutes: block_minutes - *budget,
            duration_minutes: take,
            kind,
        });
        step.remaining_minutes -= take;
        *budget -= take;
        consumed_any = true;
        if step.remaining_minutes <= 0 {
            st.milestones[mi].cursor = si + 1;
        }
    }
    // 一个块只算一块（哪怕它装了多个 step，或一个 step 被切成几段）
    if consumed_any {
        st.blocks_used = blocks_before + 1;
    }
}

/// **分配一周的整块。**
///
/// 顺序固定为 **硬锚定 → 基础量 → 跟课 → 剩余自由分配**。
/// 硬约束必须先于弹性项，否则弹性项会把可用块吃光、把硬锚定目标饿死。
///
/// step 流跨周延续：本周没做完的 step，下周接着做。
pub fn allocate_week(
    goals: &mut [GoalState],
    hard: &[Demand],
    soft: &[Demand],
    course_blocks: usize,
    slots: &[Slot],
    block_minutes: i64,
) -> Vec<AllocatedSegment> {
    // 1) 排出本周的目标队列：硬锚定（按紧迫度）→ 基础量 → 跟课
    let mut queue: VecDeque<(usize, BlockKind)> = VecDeque::new();
    let mut hard_sorted = hard.to_vec();
    hard_sorted.sort_by_key(|d| d.weeks_left);
    for d in hard_sorted {
        for _ in 0..d.blocks {
            queue.push_back((d.goal_idx, BlockKind::Hard));
        }
    }
    for d in soft {
        for _ in 0..d.blocks {
            queue.push_back((d.goal_idx, BlockKind::Base));
        }
    }
    for (i, st) in goals.iter().enumerate() {
        if st.spec.kind == GoalKind::Course {
            let n = st.spec.weekly_blocks.min(course_blocks.max(st.spec.weekly_blocks));
            for _ in 0..n {
                queue.push_back((i, BlockKind::Course));
            }
        }
    }

    // 2) 灌块：**块内可以切换目标**。
    //    真实的一天是「先做四级的听力，再切到 Rust 的复习」，
    //    不是「一个块锁死给一个目标，目标做完块就空着」。
    let mut out = Vec::new();
    for slot in slots {
        let mut budget = block_minutes;
        while budget > 0 {
            let (goal_idx, kind) = match queue.pop_front() {
                Some(x) => x,
                None => match pick_next(goals) {
                    Some(i) => (i, BlockKind::Flexible),
                    None => break, // 没事可做了
                },
            };
            let before = goals[goal_idx].remaining_minutes();
            consume_into_block(
                &mut goals[goal_idx],
                kind,
                slot,
                block_minutes,
                &mut budget,
                &mut out,
            );
            if goals[goal_idx].remaining_minutes() == before {
                // 这个目标已经做完了（没进展）→ 跳过它，避免原地打转
                continue;
            }
        }
    }

    out.sort_by_key(|b| (b.date, b.offset_minutes));
    out
}

/// 挑下一个最值得做的目标：权重 × 剩余工作量（剩余越多越该先推）。
pub fn pick_next(goals: &[GoalState]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, st) in goals.iter().enumerate() {
        if st.done() {
            continue;
        }
        // 判据是「还有未完成的 step」，不是「还剩 ≥1 整块」——
        // 后者会把跨块延续的 step 剩余部分直接丢掉（175 分钟只排 120 的那个 bug）。
        let remaining_minutes = st.remaining_minutes();
        if remaining_minutes <= 0 {
            continue;
        }
        let remaining_blocks = remaining_minutes as f64 / 120.0;
        let value = st.spec.weight * remaining_blocks.min(6.0);
        if best.is_none_or(|(_, v)| value > v) {
            best = Some((i, value));
        }
    }
    best.map(|(i, _)| i)
}

/// 可行性诊断结果。
///
/// 语义修正（2026-11-20 研究实验发现）：
/// 不能把「跨学期目标」的全量需求都算进本学期 ——
///
/// - **硬锚定**（锚点在本学期内）→ 本学期必须全做完
/// - **跟课型**（学校课程）      → 本学期固定投入 = 每周块数 × 学期周数
/// - **学期外锚定**              → 本学期只做基础量
/// - **日常型**                  → 可延到寒假，不算进"硬需求"
#[derive(Debug, Clone)]
pub struct Feasibility {
    pub available_blocks: i64,
    pub hard_need: i64,
    pub course_need: i64,
    pub mandatory_need: i64,
    pub elastic_capacity: i64,
    pub elastic_need: i64,
}

impl Feasibility {
    pub fn gap(&self) -> i64 {
        self.mandatory_need - self.available_blocks
    }

    /// 硬锚定 + 跟课型自己是否就冲突
    pub fn hard_self_conflict(&self) -> bool {
        self.mandatory_need > self.available_blocks
    }

    pub fn elastic_blocks(&self) -> i64 {
        self.elastic_capacity.max(0)
    }
}

/// 计算可行性诊断。
pub fn feasibility(
    goals: &[GoalState],
    available_blocks: i64,
    term_end: DateTime<Utc>,
    term_start: DateTime<Utc>,
    block_minutes: i64,
    base_blocks: i64,
) -> Feasibility {
    let weeks = (((term_end - term_start).num_days() as f64) / 7.0).ceil() as i64;
    let weeks = weeks.max(1);
    let blocks_of = |minutes: i64| (minutes + block_minutes - 1) / block_minutes;

    let hard_need: i64 = goals
        .iter()
        .filter(|st| {
            st.spec.kind == GoalKind::Pulse
                && st
                    .spec
                    .anchor_date()
                    .map(|a| {
                        a.and_hms_opt(0, 0, 0)
                            .map(|n| Utc.from_utc_datetime(&n))
                            .map(|dt| dt <= term_end)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
        })
        .map(|st| blocks_of(st.remaining_minutes()))
        .sum();

    let course_need: i64 = goals
        .iter()
        .filter(|st| st.spec.kind == GoalKind::Course)
        .map(|st| st.spec.weekly_blocks as i64 * weeks)
        .sum();

    let soft_need: i64 = goals
        .iter()
        .filter(|st| {
            st.spec.kind == GoalKind::Pulse
                && st
                    .spec
                    .anchor_date()
                    .map(|a| {
                        a.and_hms_opt(0, 0, 0)
                            .map(|n| Utc.from_utc_datetime(&n))
                            .map(|dt| dt > term_end)
                            .unwrap_or(false)
                    })
                    .unwrap_or(false)
        })
        .map(|_| base_blocks * weeks)
        .sum();

    let elastic_need: i64 = goals
        .iter()
        .filter(|st| st.spec.kind == GoalKind::Daily)
        .map(|st| blocks_of(st.remaining_minutes()))
        .sum();

    let mandatory_need = hard_need + course_need + soft_need;
    Feasibility {
        available_blocks,
        hard_need,
        course_need,
        mandatory_need,
        elastic_capacity: available_blocks - mandatory_need,
        elastic_need,
    }
}

/// 把每周槽位展开成具体日期（按星期的固定时刻）。
pub fn expand_slots(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    weekday: u8,
    hour: u32,
    minute: u32,
    label: &str,
) -> Vec<Slot> {
    let mut out = Vec::new();
    let mut d = start;
    while d <= end {
        if d.weekday().number_from_monday() == weekday as u32 {
            let date = d
                .date_naive()
                .and_hms_opt(hour, minute, 0)
                .map(|n| Utc.from_utc_datetime(&n))
                .expect("time");
            out.push(Slot {
                date,
                label: label.to_owned(),
            });
        }
        d += Duration::days(1);
    }
    out
}
