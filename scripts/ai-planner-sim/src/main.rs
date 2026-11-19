//! 多目标 AI 规划模拟：AI 规划、人类学习。
//!
//! 跑一遍完整的多目标排期：L0 硬锚定脉冲型倒排、L1 跟课型绑定、
//! 碎片池交给 FSRS 做复习调度、剩余整块按「进度 × 权重」做跨目标背包分配。
//!
//! 时间预算来自生产真实课表（scripts/plan-sim.py 同一份数据）：
//!   整块：周一晚 19:00-21:00 + 周日上午 10:00-12:00，各 2 小时
//!   碎片：课间缝 90 分钟/周（7 段 10-20 分钟）
//!
//! ⚠️ 目标锚点（考试日/报名截止）是**假设值**，用于演示冲突检测；
//!    真实值需从 docs/plans/portfolio.md §8 的待补清单里填。
//!
//! 跑法：cargo run --release

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use fsrs::{FSRS, MemoryState};

const BLOCK_MINUTES: i64 = 120;
const DESIRED_RETENTION: f32 = 0.90;
/// 碎片：每天 13 分钟（90min/周 ÷ 7），每张卡 10 秒
const FRAGMENT_SECONDS_PER_DAY: f64 = 13.0 * 60.0;
const SECONDS_PER_CARD: f64 = 10.0;
/// 知识库规模（背书知识点的卡片数）
const CARD_COUNT: usize = 300;

#[derive(Clone, Copy, PartialEq, Debug)]
enum GoalKind {
    /// 硬锚定脉冲型：考试/报名截止驱动，倒排
    Pulse,
    /// 跟课型：跟着校历周次走
    CourseBound,
    /// 日常型：整块推进
    Daily,
}

struct Milestone {
    name: &'static str,
    /// 完成需要的整块分钟数
    minutes: i64,
}

struct GoalSpec {
    id: &'static str,
    name: &'static str,
    kind: GoalKind,
    /// 目标权重（人给的 0-100）
    weight: f64,
    /// 硬锚点：考试日 / 报名截止
    anchor: Option<&'static str>,
    /// 每周固定占几个块（跟课型用）
    weekly_blocks: usize,
    milestones: Vec<Milestone>,
}

/// 目标集（锚点为假设值，见文件头警告）
fn goals() -> Vec<GoalSpec> {
    vec![
        GoalSpec {
            id: "cet4",
            name: "四级 CET",
            kind: GoalKind::Pulse,
            weight: 80.0,
            anchor: Some("2026-12-19"),
            weekly_blocks: 0,
            milestones: vec![
                Milestone { name: "词汇 5500 核心", minutes: 360 },
                Milestone { name: "听力精听 15 篇", minutes: 360 },
                Milestone { name: "阅读 20 篇", minutes: 480 },
                Milestone { name: "写作模板 + 15 篇", minutes: 360 },
                Milestone { name: "真题 3 套 + 模考", minutes: 480 },
            ],
        },
        GoalSpec {
            id: "hicp",
            name: "华为 HICP 证书",
            kind: GoalKind::Pulse,
            weight: 60.0,
            anchor: Some("2026-12-31"),
            weekly_blocks: 0,
            milestones: vec![
                Milestone { name: "课程 8 节", minutes: 480 },
                Milestone { name: "模拟题 + 错题", minutes: 240 },
                Milestone { name: "正式考试", minutes: 240 },
            ],
        },
        GoalSpec {
            id: "ielts",
            name: "雅思",
            kind: GoalKind::Pulse,
            weight: 70.0,
            anchor: Some("2027-02-20"),
            weekly_blocks: 0,
            milestones: vec![
                Milestone { name: "口语 12 套", minutes: 360 },
                Milestone { name: "听力 20 篇", minutes: 360 },
                Milestone { name: "阅读 15 篇", minutes: 360 },
                Milestone { name: "写作 8 篇", minutes: 360 },
                Milestone { name: "真题 2 套 + 模考", minutes: 480 },
            ],
        },
        GoalSpec {
            id: "math",
            name: "高数",
            kind: GoalKind::CourseBound,
            weight: 75.0,
            anchor: Some("2027-01-15"),
            weekly_blocks: 2, // 课前预习 + 课后习题
            milestones: vec![
                Milestone { name: "跟课预习 + 课后习题", minutes: 1440 },
                Milestone { name: "期末冲刺", minutes: 480 },
            ],
        },
        GoalSpec {
            id: "rust",
            name: "学 Rust",
            kind: GoalKind::Daily,
            weight: 50.0,
            anchor: None,
            weekly_blocks: 0,
            milestones: vec![
                Milestone { name: "M1 环境与基本语法", minutes: 240 },
                Milestone { name: "M2 所有权与借用", minutes: 360 },
                Milestone { name: "M3 类型系统", minutes: 240 },
                Milestone { name: "M4 泛型与生命周期", minutes: 240 },
                Milestone { name: "M5 动手写 CLI 工具", minutes: 360 },
            ],
        },
    ]
}

/// 状态：每个目标剩余的里程碑
struct GoalState {
    spec: GoalSpec,
    remaining: Vec<i64>,
    /// 已分配的块数
    blocks_used: u64,
}

impl GoalState {
    fn remaining_minutes(&self) -> i64 {
        self.remaining.iter().sum()
    }
    fn done(&self) -> bool {
        self.remaining.iter().all(|&m| m <= 0)
    }
}

struct Block {
    date: DateTime<Utc>,
    label: &'static str,
    goal: String,
    content: String,
    kind: &'static str,
}

/// 真实课表推出的可用整块：周一晚 + 周日上午
fn available_blocks(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(DateTime<Utc>, &'static str)> {
    let mut out = Vec::new();
    let mut d = start;
    while d <= end {
        let wd = d.weekday().num_days_from_monday();
        // 周一 → "周一晚"，周日 → "周日上午"
        if wd == 0 {
            out.push((d, "周一晚"));
        } else if wd == 6 {
            out.push((d, "周日上午"));
        }
        d += Duration::days(1);
    }
    out
}

/// 碎片段：真实课表里的课间缝（周一/三/四/五），带真实长度
fn fragment_slots(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<(DateTime<Utc>, i64)> {
    // 实测（2026-11-23 那周）：周一 16:50-17:00(10)、周三 10:20-10:40(20) 与 16:50-17:00(10)、
    // 周四 10:20-10:40(20) 与 15:20-15:30(10) 与 16:50-17:00(10)、周五 15:20-15:30(10)
    // 合计 90 分钟/周
    let pattern: [(usize, u32, u32, i64); 7] = [
        (0, 16, 50, 10),
        (2, 10, 20, 20),
        (2, 16, 50, 10),
        (3, 10, 20, 20),
        (3, 15, 20, 10),
        (3, 16, 50, 10),
        (4, 15, 20, 10),
    ];
    let mut out = Vec::new();
    let mut d = start;
    while d <= end {
        let wd = d.weekday().num_days_from_monday() as usize;
        for (p_wd, _hh, _mm, len) in pattern {
            if p_wd == wd {
                out.push((d, len));
            }
        }
        d += Duration::days(1);
    }
    out
}

/// L0 倒排：距离锚点还有几周，每周必须拿几个块
fn required_blocks_per_week(state: &GoalState, weeks_left: i64) -> usize {
    let remaining = state.remaining_minutes();
    if remaining <= 0 {
        return 0;
    }
    let weeks = weeks_left.max(1);
    let per_week = (remaining + weeks - 1) / weeks;
    ((per_week + BLOCK_MINUTES - 1) / BLOCK_MINUTES).max(1) as usize
}

fn main() {
    let fsrs = FSRS::default();
    let term_start = Utc.with_ymd_and_hms(2026, 11, 22, 0, 0, 0).unwrap();
    let term_end = Utc.with_ymd_and_hms(2027, 1, 3, 0, 0, 0).unwrap();

    let blocks_avail = available_blocks(term_start, term_end);
    let frag_slots = fragment_slots(term_start, term_end);

    println!("╔══════════════════════════════════════════════════════════════════════════════╗");
    println!("║  多目标 AI 规划模拟 —— AI 规划，人类学习                                        ║");
    println!("╚══════════════════════════════════════════════════════════════════════════════╝");
    println!(
        "学期 {} ~ {}",
        term_start.format("%Y-%m-%d"),
        term_end.format("%Y-%m-%d")
    );
    let frag_week_minutes: i64 = {
        let first_week: i64 = frag_slots
            .iter()
            .filter(|(d, _)| *d < term_start + Duration::days(7))
            .map(|(_, m)| *m)
            .sum();
        first_week
    };
    println!(
        "可用整块 {} 个（{} 分钟）｜碎片 {} 段（首周实测 {} 分钟）",
        blocks_avail.len(),
        blocks_avail.len() as i64 * BLOCK_MINUTES,
        frag_slots.len(),
        frag_week_minutes
    );
    println!("\n⚠️ 锚点为假设值（四级 12-19 / HICP 12-31 / 雅思 2027-02-20 / 高数期末 01-15）");
    println!("   目标权重与锚点是人给的，其余全部由规划器决定。\n");

    let mut states: Vec<GoalState> = goals()
        .into_iter()
        .map(|spec| GoalState {
            remaining: spec.milestones.iter().map(|m| m.minutes).collect(),
            blocks_used: 0,
            spec,
        })
        .collect();

    // ---- 碎片：FSRS 维护记忆 ----
    let cards = simulate_fragments(&fsrs, term_start, term_end, &frag_slots);

    // ---- 整块：逐周分配 ----
    let mut plan: Vec<(DateTime<Utc>, Vec<Block>)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut week_start = term_start;
    let mut week_index = 0usize;

    while week_start <= term_end {
        week_index += 1;
        let week_end = week_start + Duration::days(6);
        let mut week_blocks: Vec<Block> = Vec::new();

        let week_slots: Vec<(DateTime<Utc>, &'static str)> = blocks_avail
            .iter()
            .filter(|(d, _)| *d >= week_start && *d <= week_end)
            .cloned()
            .collect();
        let capacity = week_slots.len();

        // 1) 算本周各类目标的需求
        //    L0（硬锚定）分两种：
        //      · 锚点在本学期内 → **硬需求**，剩余时间全部要在这学期完成 → 完整倒排
        //      · 锚点在本学期外（寒假里）→ **软需求**，本学期只做基础量，其余留到锚点前的学期
        let mut hard_demands: Vec<(usize, usize, i64)> = Vec::new(); // (goal, blocks, weeks_left)
        let mut soft_demands: Vec<(usize, usize, i64)> = Vec::new();
        for (i, st) in states.iter().enumerate() {
            if st.spec.kind != GoalKind::Pulse {
                continue;
            }
            let Some(anchor) = st.spec.anchor else { continue };
            let anchor_dt = Utc.with_ymd_and_hms(
                anchor[0..4].parse().unwrap(),
                anchor[5..7].parse().unwrap(),
                anchor[8..10].parse().unwrap(),
                0,
                0,
                0,
            )
            .unwrap();
            let weeks_left = ((anchor_dt - week_start).num_days() as f64 / 7.0).ceil() as i64;
            if weeks_left <= 0 {
                continue;
            }
            if anchor_dt <= term_end {
                // 锚点在本学期内：完整倒排（硬）
                hard_demands.push((i, required_blocks_per_week(st, weeks_left), weeks_left));
            } else {
                // 锚点在学期外：本学期只保底 1 块基础（软）
                soft_demands.push((i, 1, weeks_left));
            }
        }

        // L1 跟课型：固定块数，也是刚性需求（否则期末必挂），参与冲突检测
        let course_demand: usize = states
            .iter()
            .filter(|st| st.spec.kind == GoalKind::CourseBound)
            .map(|st| st.spec.weekly_blocks)
            .sum();

        // 2) 冲突检测：硬需求加起来超了没有？
        let hard_total: usize = hard_demands.iter().map(|d| d.1).sum();
        let soft_total: usize = soft_demands.iter().map(|d| d.1).sum();
        let total_demand = hard_total + soft_total + course_demand;
        if total_demand > capacity {
            let mut detail = String::new();
            for (i, need, wl) in hard_demands.iter().chain(soft_demands.iter()) {
                detail.push_str(&format!(
                    "{} 需 {need} 块/周（还剩 {wl} 周） ",
                    states[*i].spec.name
                ));
            }
            if course_demand > 0 {
                detail.push_str(&format!("跟课型需 {course_demand} 块/周 "));
            }
            warnings.push(format!(
                "第{week_index}周（{}）：需求 {total_demand} 块 > 可用 {capacity} 块 —— {detail}",
                week_start.format("%m-%d")
            ));
        }

        // 3) 分配：先 L0（按紧迫度），再 L1（固定量），剩下的给 L2/其他

        // 3) 分配顺序：**L0 硬锚定 → L1 跟课 → L2 日常**
        //    硬约束必须先于弹性项，否则跟课型会把整块资源吃光、把硬锚定目标饿死
        //    （2026-11-19 模拟实测：先分 L1 时 L0 一个块都拿不到）
        let mut slots = week_slots.into_iter();
        let mut taken = 0usize;

        // L0 脉冲型：按需求分配（紧迫的先拿）
        // L0 硬需求（锚点在本学期内）：按紧迫度先拿
        let mut hard_sorted = hard_demands.clone();
        hard_sorted.sort_by_key(|(_, _, wl)| *wl);
        for (i, need, _) in hard_sorted {
            if taken >= capacity {
                break;
            }
            let n = need.min(capacity - taken);
            for _ in 0..n {
                let Some((date, label)) = slots.next() else { break };
                let st = &mut states[i];
                let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                    continue;
                };
                let content = st.spec.milestones[idx].name.to_owned();
                st.remaining[idx] = (st.remaining[idx] - BLOCK_MINUTES).max(0);
                st.blocks_used += 1;
                week_blocks.push(Block {
                    date,
                    label,
                    goal: st.spec.name.to_owned(),
                    content,
                    kind: "硬锚定",
                });
                taken += 1;
            }
        }

        // L0 软需求（锚点在本学期外）：只做基础量
        for (i, need, _) in soft_demands.clone() {
            if taken >= capacity {
                break;
            }
            for _ in 0..need.min(capacity - taken) {
                let Some((date, label)) = slots.next() else { break };
                let st = &mut states[i];
                let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                    continue;
                };
                let content = st.spec.milestones[idx].name.to_owned();
                st.remaining[idx] = (st.remaining[idx] - BLOCK_MINUTES).max(0);
                st.blocks_used += 1;
                week_blocks.push(Block {
                    date,
                    label,
                    goal: st.spec.name.to_owned(),
                    content: format!("{content}（打基础）"),
                    kind: "基础量",
                });
                taken += 1;
            }
        }

        // L1 跟课型：固定块数，但受剩余容量约束，且要真的扣里程碑
        for st in states.iter_mut() {
            if st.spec.kind != GoalKind::CourseBound {
                continue;
            }
            for _ in 0..st.spec.weekly_blocks {
                if taken >= capacity {
                    break;
                }
                let Some((date, label)) = slots.next() else { break };
                let Some(idx) = st.remaining.iter().position(|m| *m > 0) else {
                    continue;
                };
                let content = st.spec.milestones[idx].name.to_owned();
                st.remaining[idx] = (st.remaining[idx] - BLOCK_MINUTES).max(0);
                st.blocks_used += 1;
                week_blocks.push(Block {
                    date,
                    label,
                    goal: st.spec.name.to_owned(),
                    content: format!("{content}（跟课绑定）"),
                    kind: "跟课",
                });
                taken += 1;
            }
        }

        // 剩下的块：跨目标背包 —— 按 (权重 × 剩余里程碑) 分配给 L2 和未满足的 L0
        while taken < capacity {
            // 找"下一个最值得做的块"：权重高、还有未完成里程碑的目标
            let mut best: Option<(usize, f64)> = None;
            for (i, st) in states.iter().enumerate() {
                if st.done() {
                    continue;
                }
                let remaining_blocks = st.remaining_minutes() as f64 / BLOCK_MINUTES as f64;
                if remaining_blocks < 1.0 {
                    continue;
                }
                // 边际价值 ∝ 权重 × 剩余工作量（剩余越多越该先推）
                let value = st.spec.weight * remaining_blocks.min(6.0);
                if best.is_none_or(|(_, v)| value > v) {
                    best = Some((i, value));
                }
            }
            let Some((i, _)) = best else { break };
            let Some((date, label)) = slots.next() else { break };
            let st = &mut states[i];
            let idx = st.remaining.iter().position(|m| *m > 0);
            let Some(idx) = idx else { continue };
            let content = st.spec.milestones[idx].name.to_owned();
            st.remaining[idx] -= BLOCK_MINUTES;
            if st.remaining[idx] < 0 {
                st.remaining[idx] = 0;
            }
            st.blocks_used += 1;
            let kind = match st.spec.kind {
                GoalKind::Pulse => "硬锚定(补) ",
                GoalKind::CourseBound => "跟课",
                GoalKind::Daily => "日常",
            };
            week_blocks.push(Block {
                date,
                label,
                goal: st.spec.name.to_owned(),
                content,
                kind,
            });
            taken += 1;
        }

        week_blocks.sort_by_key(|b| b.date);
        plan.push((week_start, week_blocks));
        week_start += Duration::days(7);
    }

    // ---- 输出 ----
    for (ws, wblocks) in &plan {
        println!(
            "\n━━ 第{}周（{} ~ {}）· {} 块 ━━",
            (*ws - term_start).num_days() as i64 / 7 + 1,
            ws.format("%m-%d"),
            (*ws + Duration::days(6)).format("%m-%d"),
            wblocks.len()
        );
        for b in wblocks {
            println!(
                "   {} {}  {:<14} [{}] {}",
                b.date.format("%m-%d"),
                b.label,
                b.goal,
                b.kind,
                b.content
            );
        }
    }

    println!("\n\n══ 期末状态 ══");
    for st in &states {
        let total: i64 = st.spec.milestones.iter().map(|m| m.minutes).sum();
        let left = st.remaining_minutes();
        let pct = if total > 0 {
            (total - left) as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "  {:<16} 进度 {:>5.1}%  用块 {:>3}  剩余 {} 分钟{}",
            st.spec.name,
            pct,
            st.blocks_used,
            left,
            if left > 0 { "  ← 未完成" } else { "" }
        );
    }

    println!("\n══ 冲突告警 ══");
    if warnings.is_empty() {
        println!("  （无）");
    } else {
        for w in &warnings {
            println!("  ⚠ {w}");
        }
    }

    // ---- 可行性诊断 ----
    let total_blocks_avail = blocks_avail.len() as i64;
    let total_need: i64 = states
        .iter()
        .map(|st| (st.remaining_minutes() + st.blocks_used as i64 * BLOCK_MINUTES) / BLOCK_MINUTES)
        .sum();
    let hard_need: i64 = states
        .iter()
        .filter(|st| {
            st.spec.kind == GoalKind::Pulse
                && st
                    .spec
                    .anchor
                    .map(|a| {
                        let ad = Utc.with_ymd_and_hms(
                            a[0..4].parse().unwrap(),
                            a[5..7].parse().unwrap(),
                            a[8..10].parse().unwrap(),
                            0, 0, 0,
                        )
                        .unwrap();
                        ad <= term_end
                    })
                    .unwrap_or(false)
        })
        .map(|st| (st.remaining_minutes() + st.blocks_used as i64 * BLOCK_MINUTES) / BLOCK_MINUTES)
        .sum();
    let course_need: i64 = states
        .iter()
        .filter(|st| st.spec.kind == GoalKind::CourseBound)
        .map(|st| (st.remaining_minutes() + st.blocks_used as i64 * BLOCK_MINUTES) / BLOCK_MINUTES)
        .sum();

    println!("\n══ 可行性诊断 ══");
    println!("  学期内可用整块：{total_blocks_avail} 块");
    println!("  全目标总需求：  {total_need} 块");
    println!("  ├ 硬锚定（锚点在本学期内）：{hard_need} 块");
    println!("  ├ 跟课型（高数）：          {course_need} 块");
    println!("  └ 弹性（雅思基础量 + Rust）：{} 块", total_need - hard_need - course_need);
    let gap = total_need - total_blocks_avail;
    if gap > 0 {
        println!("  ⚠ 缺口 {gap} 块 —— 需求是可用量的 {:.1} 倍", total_need as f64 / total_blocks_avail as f64);
        if hard_need > total_blocks_avail {
            println!("  ⚠⚠ 硬锚定目标自己就冲突：即使放弃全部弹性目标，仍缺 {} 块", hard_need - total_blocks_avail);
            println!("     → 至少有一个硬锚定目标必须改期（换考期 / 推迟报名），");
            println!("       这不是规划能解决的，是目标组合本身的问题。");
        } else {
            println!("  → 硬需求可行，但弹性目标被完全挤出 —— 跟课型与日常型拿不到任何块。");
        }
    } else {
        println!("  ✓ 目标组合可行");
    }

    // ---- 取舍建议 ----
    println!("\n══ 取舍建议（AI 该给人什么选项）══");
    let mut options: Vec<(String, f64, i64, f64)> = Vec::new(); // (目标, 权重, 可释放块, 性价比)
    for st in &states {
        let used = st.blocks_used as i64;
        let weekly_blocks = match st.spec.kind {
            GoalKind::CourseBound => st.spec.weekly_blocks as i64,
            GoalKind::Pulse => {
                if st.remaining_minutes() > 0 {
                    2
                } else {
                    1
                }
            }
            GoalKind::Daily => 0,
        };
        let releasable = if st.spec.kind == GoalKind::Daily {
            used
        } else {
            weekly_blocks * 6
        };
        if releasable > 0 {
            let efficiency = releasable as f64 / st.spec.weight;
            options.push((st.spec.name.to_owned(), st.spec.weight, releasable, efficiency));
        }
    }
    options.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
    for (name, weight, rel, eff) in &options {
        println!("  · 暂缓「{name}」→ 释放约 {rel} 块（权重 {weight}，性价比 {eff:.2} 块/权重）");
    }
    println!("  → 单靠暂缓一个目标仍缺 {} 块，说明必须组合取舍或改期。", (gap - options.first().map(|o| o.2).unwrap_or(0)).max(0));

    println!("\n══ 碎片复习（FSRS v7）══");
    println!(
        "  {} 天，每天 {} 分钟预算｜总复习 {} 次｜期末留存 {:.1}%",
        cards.days, FRAGMENT_SECONDS_PER_DAY / 60.0, cards.total_reviews, cards.retention * 100.0
    );
    println!("  碎片总时长 {} 分钟（实测课表：90 分钟/周）", cards.total_minutes);

    println!("\n══ 结论 ══");
    let total_blocks: u64 = states.iter().map(|s| s.blocks_used).sum();
    println!("  学期内整块总产出：{total_blocks} 块");
    println!(
        "  硬锚定目标未完成 → 需要跨到寒假（寒假 8 周 × 8 块 = 64 块）"
    );
}

struct FragResult {
    days: usize,
    total_reviews: u64,
    retention: f32,
    total_minutes: i64,
}

/// 碎片复习模拟：真 FSRS 调度 + 每日时间预算
fn simulate_fragments(
    fsrs: &FSRS,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    slots: &[(DateTime<Utc>, i64)],
) -> FragResult {
    let mut rng = Rng(42);
    let mut cards: Vec<(Option<MemoryState>, Option<DateTime<Utc>>, u32)> = vec![(None, None, 0); CARD_COUNT];
    let mut day_index = 0usize;
    let mut cur = start;
    let mut total_reviews = 0u64;
    let mut recalls = 0u64;
    let mut attempts = 0u64;
    let mut total_minutes = 0i64;

    // 第一天引入全部新卡（整块学完）
    for c in cards.iter_mut() {
        let first = fsrs.next_states(None, DESIRED_RETENTION, 0).unwrap().good;
        c.0 = Some(first.memory);
        c.1 = Some(start);
        c.2 = 1;
    }

    while cur <= end {
        let today_slots: Vec<i64> = slots
            .iter()
            .filter(|(d, _)| d.date_naive() == cur.date_naive())
            .map(|(_, m)| *m)
            .collect();
        let minutes_today: i64 = today_slots.iter().sum();
        total_minutes += minutes_today;
        let max_cards = (minutes_today as f64 * 60.0 / SECONDS_PER_CARD) as usize;

        // 到期卡
        let mut due: Vec<usize> = (0..cards.len())
            .filter(|&i| {
                cards[i]
                    .1
                    .map(|lr| {
                        let e = (cur - lr).num_seconds() as f32 / 86_400.0;
                        fsrs.current_retrievability(cards[i].0.unwrap(), e) < 0.9
                    })
                    .unwrap_or(false)
            })
            .collect();
        // FSRS 策略：最接近遗忘的先复习
        due.sort_by(|&a, &b| {
            let ra = {
                let e = (cur - cards[a].1.unwrap()).num_seconds() as f32 / 86_400.0;
                fsrs.current_retrievability(cards[a].0.unwrap(), e)
            };
            let rb = {
                let e = (cur - cards[b].1.unwrap()).num_seconds() as f32 / 86_400.0;
                fsrs.current_retrievability(cards[b].0.unwrap(), e)
            };
            ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
        });

        for &i in due.iter().take(max_cards) {
            let e = (cur - cards[i].1.unwrap()).num_seconds() as f32 / 86_400.0;
            let r = fsrs.current_retrievability(cards[i].0.unwrap(), e);
            let recalled = rng.next_f64() < r as f64;
            let next = fsrs
                .next_states(cards[i].0, DESIRED_RETENTION, e.floor().max(0.0) as u32)
                .unwrap();
            let ns = if recalled { next.good } else { next.again };
            cards[i].0 = Some(ns.memory);
            cards[i].1 = Some(cur);
            cards[i].2 += 1;
            total_reviews += 1;
            attempts += 1;
            if recalled {
                recalls += 1;
            }
        }

        day_index += 1;
        cur += Duration::days(1);
    }

    FragResult {
        days: day_index,
        total_reviews,
        retention: if attempts > 0 { recalls as f32 / attempts as f32 } else { 0.0 },
        total_minutes,
    }
}

struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}
