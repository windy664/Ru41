//! 回归测试：把 2026-11-19 那次模拟抓出来的 3 个 bug 固化成不变量。
//!
//! bug 1（分配顺序）是最要命的：先分弹性项（跟课型），它固定占满每周可用块，
//! 把硬锚定目标完全饿死。这违反 Reclaim 的核心规则（硬约束永不被挤占），
//! 而且**只有真跑一遍才会暴露** —— 纸面上看不出来。

use ai_planner_sim::config::{GoalKind, GoalSpec, MilestoneSpec};
use ai_planner_sim::planner::{
    allocate_week, compute_demands, course_demand, feasibility, BlockKind, GoalState, Slot,
};
use chrono::{DateTime, Duration, TimeZone, Utc};

const BLOCK_MIN: i64 = 120;

fn dt(y: i32, m: u32, d: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
}

fn pulse_goal(id: &str, name: &str, weight: f64, anchor: &str, minutes: i64) -> GoalSpec {
    GoalSpec {
        id: id.into(),
        name: name.into(),
        kind: GoalKind::Pulse,
        weight,
        anchor: Some(anchor.into()),
        weekly_blocks: 0,
        milestones: vec![MilestoneSpec {
            name: "里程碑".into(),
            minutes,
        }],
    }
}

fn course_goal(name: &str, weight: f64, weekly_blocks: usize) -> GoalSpec {
    GoalSpec {
        id: "course".into(),
        name: name.into(),
        kind: GoalKind::Course,
        weight,
        anchor: None,
        weekly_blocks,
        milestones: vec![MilestoneSpec {
            name: "跟课".into(),
            minutes: 1440,
        }],
    }
}

fn daily_goal(name: &str, weight: f64, minutes: i64) -> GoalSpec {
    GoalSpec {
        id: "daily".into(),
        name: name.into(),
        kind: GoalKind::Daily,
        weight,
        anchor: None,
        weekly_blocks: 0,
        milestones: vec![MilestoneSpec {
            name: "日常块".into(),
            minutes,
        }],
    }
}

fn slots(n: usize, start: DateTime<Utc>) -> Vec<Slot> {
    (0..n)
        .map(|i| Slot {
            date: start + Duration::days(i as i64),
            label: "槽".into(),
        })
        .collect()
}

/// **bug 1 回归**：硬锚定目标不能被跟课型（弹性项）饿死。
///
/// 场景：每周只有 2 个块；跟课型固定要 2 个块；硬锚定目标也需要块。
/// 错的做法（先分 L1）会让硬锚定目标一个块都拿不到。
#[test]
fn hard_constraint_is_not_starved_by_flexible_goal() {
    let week_start = dt(2026, 11, 22);
    let term_end = dt(2027, 1, 3);
    let mut goals = vec![
        GoalState::new(course_goal("高数", 75.0, 2)),
        GoalState::new(pulse_goal("cet4", "四级", 80.0, "2026-12-19", 17 * BLOCK_MIN)),
    ];

    let (hard, soft) = compute_demands(&goals, week_start, term_end, BLOCK_MIN, 1);
    assert_eq!(hard.len(), 1, "锚点在本学期内 → 应算硬需求");
    assert!(soft.is_empty());

    let course = course_demand(&goals);
    let blocks = allocate_week(
        &mut goals,
        &hard,
        &soft,
        course,
        &slots(2, week_start),
        BLOCK_MIN,
    );

    let hard_blocks = blocks.iter().filter(|b| b.kind == BlockKind::Hard).count();
    let course_blocks = blocks.iter().filter(|b| b.kind == BlockKind::Course).count();
    // 硬需求是 5 块/周 > 容量 2 块 → 硬需求独占全部容量，跟课型让位。
    // 这就是 bug 1 的不变量：**硬约束优先，弹性项让位**。
    assert_eq!(
        hard_blocks, 2,
        "硬锚定目标必须优先拿到全部可用块（bug 1 回归）"
    );
    assert_eq!(course_blocks, 0, "跟课型必须让位给硬约束");
}

/// **bug 2 回归**：分配一块就要扣减里程碑。
#[test]
fn allocating_consumes_milestone_minutes() {
    let week_start = dt(2026, 11, 22);
    let term_end = dt(2027, 1, 3);
    let mut goals = vec![GoalState::new(pulse_goal(
        "cet4",
        "四级",
        80.0,
        "2026-12-19",
        17 * BLOCK_MIN,
    ))];
    let before = goals[0].remaining_minutes();

    let (hard, soft) = compute_demands(&goals, week_start, term_end, BLOCK_MIN, 1);
    allocate_week(&mut goals, &hard, &soft, 0, &slots(2, week_start), BLOCK_MIN);

    assert_eq!(
        goals[0].remaining_minutes(),
        before - 2 * BLOCK_MIN,
        "每分配一块应扣 120 分钟（bug 2 回归）"
    );
    assert_eq!(goals[0].blocks_used, 2);
}

/// **bug 3 回归**：锚点在本学期外的目标，本学期只做基础量，不按完整倒排占块。
#[test]
fn goal_with_anchor_outside_term_only_gets_base_blocks() {
    let week_start = dt(2026, 11, 22);
    let term_end = dt(2027, 1, 3);
    let goals = vec![GoalState::new(pulse_goal(
        "ielts",
        "雅思",
        70.0,
        "2027-02-20", // 学期外
        16 * BLOCK_MIN,
    ))];

    let (hard, soft) = compute_demands(&goals, week_start, term_end, BLOCK_MIN, 1);
    assert!(hard.is_empty(), "学期外锚点不该算硬需求");
    assert_eq!(soft.len(), 1);
    assert_eq!(soft[0].blocks, 1, "学期外锚点本学期只做 1 块基础量");
}

/// 可行性诊断：硬锚定目标自己就冲突时要能识别出来。
#[test]
fn feasibility_detects_hard_self_conflict() {
    let term_end = dt(2027, 1, 3);
    let goals = vec![
        GoalState::new(pulse_goal("cet4", "四级", 80.0, "2026-12-19", 17 * BLOCK_MIN)),
        GoalState::new(pulse_goal("hicp", "HICP", 60.0, "2026-12-31", 8 * BLOCK_MIN)),
        GoalState::new(daily_goal("Rust", 50.0, 12 * BLOCK_MIN)),
    ];
    let feas = feasibility(&goals, 13, term_end, dt(2026, 11, 22), BLOCK_MIN, 1);
    assert!(feas.hard_self_conflict(), "硬需求 25 块 > 可用 13 块 → 应判为自冲突");
    assert_eq!(feas.hard_need, 25, "四级 17 + HICP 8，两个都是学期内锚点");
    assert_eq!(feas.available_blocks, 13);
    assert!(feas.gap() > 0);
}

/// 可行性诊断：需求够时不该误报。
#[test]
fn feasibility_ok_when_enough_capacity() {
    let term_end = dt(2027, 1, 3);
    let goals = vec![GoalState::new(pulse_goal(
        "cet4",
        "四级",
        80.0,
        "2026-12-19",
        4 * BLOCK_MIN,
    ))];
    let feas = feasibility(&goals, 13, term_end, dt(2026, 11, 22), BLOCK_MIN, 1);
    assert!(!feas.hard_self_conflict());
    assert!(feas.gap() <= 0, "4 块需求 vs 13 块可用 → 不该有缺口");
}

/// 剩余整块的取舍：权重 × 剩余工作量决定下一个给谁。
#[test]
fn pick_next_prefers_higher_weight() {
    let goals = vec![
        GoalState::new(daily_goal("低权重", 10.0, 10 * BLOCK_MIN)),
        GoalState::new(daily_goal("高权重", 90.0, 10 * BLOCK_MIN)),
    ];
    assert_eq!(ai_planner_sim::planner::pick_next(&goals), Some(1));
}

/// 目标全部做完时不该再被选中。
#[test]
fn pick_next_skips_finished_goals() {
    let mut goals = vec![GoalState::new(daily_goal("做完的", 90.0, 120))];
    allocate_week(
        &mut goals,
        &[],
        &[],
        0,
        &slots(1, dt(2026, 11, 22)),
        BLOCK_MIN,
    );
    assert!(goals[0].done());
    assert_eq!(ai_planner_sim::planner::pick_next(&goals), None);
}
