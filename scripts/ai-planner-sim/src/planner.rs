//! 规划核心：纯函数，不碰 IO。
//!
//! 上一轮模拟抓出三个 bug，其中最要命的是「分配顺序错」——先分弹性项（跟课型），
//! 它固定占满每周可用块，把硬锚定目标饿死。**硬约束必须先于弹性项分配。**
//! 这里把它变成一条可测的不变量（见 tests/planner.rs）。

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};

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

#[derive(Debug, Clone)]
pub struct AllocatedBlock {
    pub date: DateTime<Utc>,
    pub label: String,
    pub goal_idx: usize,
    pub goal_name: String,
    pub content: String,
    pub kind: BlockKind,
}

/// 一个可用整块槽位（具体到某天）。
#[derive(Debug, Clone)]
pub struct Slot {
    pub date: DateTime<Utc>,
    pub label: String,
}

/// 目标状态：里程碑剩余量 + 已用块数。
#[derive(Debug, Clone)]
pub struct GoalState {
    pub spec: GoalSpec,
    pub remaining: Vec<i64>,
    pub blocks_used: u64,
}

impl GoalState {
    pub fn new(spec: GoalSpec) -> Self {
        let remaining = spec.milestones.iter().map(|m| m.minutes).collect();
        Self {
            spec,
            remaining,
            blocks_used: 0,
        }
    }

    pub fn remaining_minutes(&self) -> i64 {
        self.remaining.iter().sum()
    }

    pub fn done(&self) -> bool {
        self.remaining.iter().all(|&m| m <= 0)
    }

    /// 已经投入的分钟数（已用块 + 里程碑消耗）
    pub fn invested_minutes(&self, block_minutes: i64) -> i64 {
        self.blocks_used as i64 * block_minutes
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
///
/// - 锚点**在本学期内** → 硬需求：剩余时间必须在这学期完成 → 完整倒排
/// - 锚点**在本学期外** → 软需求：本学期只做基础量（`base_blocks`），其余留到锚点前的学期
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
            let need = required_blocks_per_week(st, weeks_left, block_minutes);
            hard.push(Demand {
                goal_idx: i,
                blocks: need,
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
pub fn required_blocks_per_week(
    st: &GoalState,
    weeks_left: i64,
    block_minutes: i64,
) -> usize {
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

/// **分配一周的整块。**
///
/// 顺序固定为 **硬锚定 → 基础量 → 跟课 → 剩余自由分配**。
/// 硬约束必须先于弹性项，否则弹性项会把可用块吃光、把硬锚定目标饿死
/// （2026-11-19 模拟实测的 bug 1）。
///
/// 返回分配的块；`goals` 的 `remaining` 与 `blocks_used` 会被就地更新。
pub fn allocate_week(
    goals: &mut [GoalState],
    hard: &[Demand],
    soft: &[Demand],
    course_blocks: usize,
    slots: &[Slot],
    block_minutes: i64,
) -> Vec<AllocatedBlock> {
    let capacity = slots.len();
    let mut out = Vec::new();
    let mut slot_iter = slots.iter();
    let mut taken = 0usize;

    // 1) 硬锚定：按紧迫度（weeks_left 小）先拿
    let mut hard_sorted = hard.to_vec();
    hard_sorted.sort_by_key(|d| d.weeks_left);
    for d in hard_sorted {
        if taken >= capacity {
            break;
        }
        for _ in 0..d.blocks.min(capacity - taken) {
            let Some(slot) = slot_iter.next() else { break };
            let st = &mut goals[d.goal_idx];
            let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                continue; // 该目标已无里程碑，块还回去
            };
            let content = st.spec.milestones[idx].name.clone();
            st.remaining[idx] = (st.remaining[idx] - block_minutes).max(0);
            st.blocks_used += 1;
            out.push(AllocatedBlock {
                date: slot.date,
                label: slot.label.clone(),
                goal_idx: d.goal_idx,
                goal_name: st.spec.name.clone(),
                content,
                kind: BlockKind::Hard,
            });
            taken += 1;
        }
    }

    // 2) 基础量（锚点在本学期外的目标）
    for d in soft {
        if taken >= capacity {
            break;
        }
        for _ in 0..d.blocks.min(capacity - taken) {
            let Some(slot) = slot_iter.next() else { break };
            let st = &mut goals[d.goal_idx];
            let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                continue;
            };
            let content = st.spec.milestones[idx].name.clone();
            st.remaining[idx] = (st.remaining[idx] - block_minutes).max(0);
            st.blocks_used += 1;
            out.push(AllocatedBlock {
                date: slot.date,
                label: slot.label.clone(),
                goal_idx: d.goal_idx,
                goal_name: st.spec.name.clone(),
                content: format!("{content}（打基础）"),
                kind: BlockKind::Base,
            });
            taken += 1;
        }
    }

    // 3) 跟课型：固定块数，受剩余容量约束
    for st in goals.iter_mut() {
        if st.spec.kind != GoalKind::Course {
            continue;
        }
        for _ in 0..course_blocks.min(st.spec.weekly_blocks) {
            if taken >= capacity {
                break;
            }
            let Some(slot) = slot_iter.next() else { break };
            let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                continue;
            };
            let content = st.spec.milestones[idx].name.clone();
            st.remaining[idx] = (st.remaining[idx] - block_minutes).max(0);
            st.blocks_used += 1;
            out.push(AllocatedBlock {
                date: slot.date,
                label: slot.label.clone(),
                goal_idx: usize::MAX, // 跟课型可能不止一个，这里不用于索引
                goal_name: st.spec.name.clone(),
                content: format!("{content}（跟课绑定）"),
                kind: BlockKind::Course,
            });
            taken += 1;
        }
    }

    // 4) 剩余块：跨目标背包，按「权重 × 剩余工作量」挑最值得做的
    while taken < capacity {
        let Some(slot) = slot_iter.next() else { break };
        let best = pick_next(goals);
        let Some(goal_idx) = best else { break };
        let st = &mut goals[goal_idx];
        let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
            continue;
        };
        let content = st.spec.milestones[idx].name.clone();
        st.remaining[idx] = (st.remaining[idx] - block_minutes).max(0);
        st.blocks_used += 1;
        out.push(AllocatedBlock {
            date: slot.date,
            label: slot.label.clone(),
            goal_idx,
            goal_name: st.spec.name.clone(),
            content,
            kind: BlockKind::Flexible,
        });
        taken += 1;
    }

    out.sort_by_key(|b| b.date);
    out
}

/// 挑下一个最值得做的目标：权重 × 剩余工作量（剩余越多越该先推）。
pub fn pick_next(goals: &[GoalState]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, st) in goals.iter().enumerate() {
        if st.done() {
            continue;
        }
        let remaining_blocks = st.remaining_minutes() as f64 / 120.0;
        if remaining_blocks < 1.0 {
            continue;
        }
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
/// 上一版把「跨学期目标」的全量需求都算进本学期，于是
/// 「高数」这种**学校课程**（不是可选项，必然发生）也显示为"不可行"，
/// 「雅思」这种锚点在寒假的��被误判。正确语义：
///
/// - **硬锚定**（锚点在本学期内）→ 本学期必须全做完
/// - **跟课型**（学校课程）      → 本学期固定投入 = 每周块数 × 学期周数
/// - **学期外锚定**              → 本学期只做基础量
/// - **日常型**                  → 可延到寒假，不算进"硬需求"
#[derive(Debug, Clone)]
pub struct Feasibility {
    /// 本学期可用整块
    pub available_blocks: i64,
    /// 硬锚定需求（必须在本学期完成）
    pub hard_need: i64,
    /// 跟课型投入（学校课程，必然发生）
    pub course_need: i64,
    /// 硬需求 + 跟课需求之和
    pub mandatory_need: i64,
    /// 留给弹性目标的块数（可为负 = 硬约束已冲突）
    pub elastic_capacity: i64,
    /// 弹性目标的全量需求（仅作参考）
    pub elastic_need: i64,
}

impl Feasibility {
    pub fn gap(&self) -> i64 {
        // 缺口只看"硬需求 + 跟课"是否超容量；弹性目标可以延后
        self.mandatory_need - self.available_blocks
    }

    /// 硬锚定 + 跟课型自己是否就冲突（放弃全部弹性目标也放不下）
    pub fn hard_self_conflict(&self) -> bool {
        self.mandatory_need > self.available_blocks
    }

    /// 弹性目标（日常型）在本学期能拿到多少块
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
        .map(|st| { let t: i64 = st.spec.milestones.iter().map(|m| m.minutes).sum(); (t + block_minutes - 1) / block_minutes })
        .sum();

    // 跟课型：每周固定块数 × 学期周数
    let course_need: i64 = goals
        .iter()
        .filter(|st| st.spec.kind == GoalKind::Course)
        .map(|st| st.spec.weekly_blocks as i64 * weeks)
        .sum();

    // 学期外锚定：本学期只做基础量
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

    // 弹性（日常型）：可延后，只作参考
    let elastic_need: i64 = goals
        .iter()
        .filter(|st| st.spec.kind == GoalKind::Daily)
        .map(|st| { let t: i64 = st.spec.milestones.iter().map(|m| m.minutes).sum(); (t + block_minutes - 1) / block_minutes })
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
