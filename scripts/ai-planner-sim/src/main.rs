//! 多目标 AI 规划模拟：AI 规划，人类学习。
//!
//! 用法：
//!   cargo run --release                          # 用 config.toml 跑一遍
//!   cargo run --release -- --config x.toml       # 指定配置
//!   cargo run --release -- --sweep cet4          # 研究模式：扫该目标的「提前多久开始」
//!
//! 配置见 config.example.toml。锚点、权重、槽位全部外置 —— 改配置就能重跑。

mod config;
mod plan;
mod planner;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use std::collections::BTreeMap;
use std::path::PathBuf;

use config::{Config, GoalKind};
use planner::{
    allocate_week, compute_demands, course_demand, feasibility, BlockKind, GoalState, Slot,
};

struct FragResult {
    days: usize,
    total_reviews: u64,
    retention: f32,
    total_minutes: i64,
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

/// 从块起点算的分钟偏移 → HH:MM
fn fmt_min(offset: i64) -> String {
    format!("{:02}:{:02}", offset / 60, offset % 60)
}

fn parse_hm(s: &str) -> (u32, u32) {
    let (h, m) = s.split_once(':').expect("HH:MM");
    (h.parse().expect("hour"), m.parse().expect("minute"))
}

/// 跑一遍规划。`anchor_override` = (目标 id, 提前几周) —— 研究模式用。
fn run_plan(
    cfg: &Config,
    slots: &[Slot],
    term_start: DateTime<Utc>,
    term_end: DateTime<Utc>,
    anchor_override: Option<(&str, i64)>,
) -> (Vec<Vec<planner::AllocatedSegment>>, Vec<GoalState>, Vec<String>) {
    // 应用 anchor 覆盖（提前 N 周）
    let mut specs = cfg.goals.clone();
    if let Some((id, weeks_earlier)) = anchor_override
        && let Some(g) = specs.iter_mut().find(|g| g.id == id)
        && let Some(anchor) = g.anchor_date()
    {
        let shifted = anchor - Duration::weeks(weeks_earlier);
        g.anchor = Some(shifted.format("%Y-%m-%d").to_string());
    }

    let mut goals: Vec<GoalState> = specs.into_iter().map(GoalState::new).collect();
    let mut warnings: Vec<String> = Vec::new();
    let mut by_week: Vec<Vec<planner::AllocatedSegment>> = Vec::new();
    let mut week_start = term_start;
    let mut week_index = 0usize;

    while week_start <= term_end {
        week_index += 1;
        let week_end = week_start + Duration::days(6);
        let week_slots: Vec<Slot> = slots
            .iter()
            .filter(|s| s.date >= week_start && s.date <= week_end)
            .cloned()
            .collect();
        let capacity = week_slots.len();

        let (hard, soft) = compute_demands(&goals, week_start, term_end, cfg.block_minutes, 1);
        let course = course_demand(&goals);

        let total_need: usize = hard.iter().map(|d| d.blocks).sum::<usize>()
            + soft.iter().map(|d| d.blocks).sum::<usize>()
            + course;
        if total_need > capacity {
            let mut detail = String::new();
            for d in hard.iter().chain(soft.iter()) {
                detail.push_str(&format!(
                    "{} 需 {} 块/周（还剩 {} 周） ",
                    goals[d.goal_idx].spec.name, d.blocks, d.weeks_left
                ));
            }
            if course > 0 {
                detail.push_str(&format!("跟课型需 {course} 块/周 "));
            }
            warnings.push(format!(
                "第{week_index}周（{}）：需求 {total_need} 块 > 可用 {capacity} 块 —— {detail}",
                week_start.format("%m-%d")
            ));
        }

        let blocks = allocate_week(
            &mut goals,
            &hard,
            &soft,
            course,
            &week_slots,
            cfg.block_minutes,
        );
        by_week.push(blocks);
        week_start += Duration::days(7);
    }
    (by_week, goals, warnings)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut config_path = PathBuf::from("config.toml");
    let mut sweep: Option<String> = None;
    let mut combinations = false;
    let mut plan_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = PathBuf::from(args.next().expect("--config 需要路径")),
            "--sweep" => sweep = Some(args.next().expect("--sweep 需要目标 id")),
            "--combinations" => combinations = true,
            "--plan" => plan_path = Some(PathBuf::from(args.next().expect("--plan 需要路径"))),
            "--help" | "-h" => {
                println!("用法: ai-planner-sim [--config <path>] [--sweep <goal_id>]");
                println!("  --sweep        扫「该目标提前多少周开始」对其他目标的影响");
                println!("  --combinations 枚举目标子集，找出这学期可行的组合");
                println!("  --plan <path>  读长期规划文档（战略层产物）→ 报告待拆解 → 装箱");
                return Ok(());
            }
            other => anyhow::bail!("未知参数: {other}"),
        }
    }
    let cfg = Config::load(&config_path)?;
    let term_start = cfg.term_start();
    let term_end = cfg.term_end();

    // 展开槽位与碎片
    let mut slots: Vec<Slot> = Vec::new();
    for b in &cfg.blocks {
        let (h, m) = parse_hm(&b.start);
        slots.extend(planner::expand_slots(
            term_start,
            term_end,
            b.weekday,
            h,
            m,
            &b.label,
        ));
    }
    slots.sort_by_key(|s| s.date);

    let mut frag_by_day: BTreeMap<DateTime<Utc>, i64> = BTreeMap::new();
    for f in &cfg.fragment_slots {
        let (h, m) = parse_hm(&f.start);
        let mut d = term_start;
        while d <= term_end {
            if d.weekday().number_from_monday() == f.weekday as u32 {
                let date = d
                    .date_naive()
                    .and_hms_opt(h, m, 0)
                    .map(|n| Utc.from_utc_datetime(&n))
                    .expect("time");
                *frag_by_day.entry(date).or_insert(0) += f.minutes;
            }
            d += Duration::days(1);
        }
    }

    // ---------- 长期规划文档模式 ----------
    if let Some(path) = plan_path {
        let markdown = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("读 {} 失败: {e}", path.display()))?;
        let parsed = plan::parse_plan(&markdown).map_err(|e| anyhow::anyhow!("解析失败: {e}"))?;

        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║  长期规划文档 → 短期任务 → 时间槽                                              ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!("文档：{}", path.display());
        println!(
            "目标：{}（kind={:?} 权重={} 锚点={}）",
            parsed.spec.name, parsed.spec.kind, parsed.spec.weight,
            parsed.spec.anchor.clone().unwrap_or_else(|| "无".into())
        );
        println!("\n── 验收标准（人写的，决定「做完没有」）──");
        for a in &parsed.acceptance {
            println!("  □ {a}");
        }
        println!("\n── 里程碑 ──");
        for m in &parsed.spec.milestones {
            println!(
                "  {}（{} 分钟，{} 步）",
                m.name,
                m.steps.iter().map(|s| s.minutes).sum::<i64>(),
                m.steps.len()
            );
            for s in &m.steps {
                println!("      · {} —— {} 分钟", s.name, s.minutes);
            }
        }
        if !parsed.needs_llm_steps.is_empty() {
            println!("\n── ⚠ 待 AI 拆解（这些里程碑还没有步骤）──");
            for m in &parsed.needs_llm_steps {
                println!("  · {m}");
            }
            println!("  → AI 应为每个里程碑产出有序 step 流（名字 + 时长），写回文档等人审。");
            println!("  → 在此之前无法装箱：算法只装箱，不猜时长。");
        }
        if !parsed.unparsed.is_empty() {
            println!("\n── 未解析的行（需检查）──");
            for u in &parsed.unparsed {
                println!("  ? {u}");
            }
        }
        if !parsed.needs_llm_steps.is_empty() {
            return Ok(());
        }

        // 有完整 steps → 用它跑装箱
        let mut cfg = cfg.clone();
        cfg.goals = vec![parsed.spec.clone()];
        println!("\n（steps 完整，开始装箱）");
        return run_with_config(&cfg, &slots, &frag_by_day, &path.display().to_string(), term_start, term_end);
    }

    // ---------- 研究模式：扫「提前多久开始」 ----------
    if let Some(goal_id) = sweep {
        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║  研究模式：扫「{goal_id}」提前多少周开始，对其他目标的影响                       ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        let base = cfg
            .goals
            .iter()
            .find(|g| g.id == goal_id)
            .and_then(|g| g.anchor.clone())
            .unwrap_or_default();
        println!("原锚点：{base}\n");
        println!(
            "{:<12}{:<14}{:<14}{:<14}{:<10}",
            "提前周数", "该目标用块", "跟课型用块", "日常型用块", "仍有冲突"
        );
        println!("{}", "-".repeat(68));
        for weeks in 0..=13i64 {
            let (_, goals, warnings) =
                run_plan(&cfg, &slots, term_start, term_end, Some((goal_id.as_str(), weeks)));
            let idx = cfg.goal_index(&goal_id).unwrap_or(0);
            let target_blocks = goals[idx].blocks_used;
            let course_blocks: u64 = goals
                .iter()
                .filter(|g| g.spec.kind == GoalKind::Course)
                .map(|g| g.blocks_used)
                .sum();
            let daily_blocks: u64 = goals
                .iter()
                .filter(|g| g.spec.kind == GoalKind::Daily)
                .map(|g| g.blocks_used)
                .sum();
            println!(
                "{:<14}{:<14}{:<14}{:<14}{:<10}",
                format!("{weeks} 周"),
                target_blocks,
                course_blocks,
                daily_blocks,
                if warnings.is_empty() { "否" } else { "是" }
            );
        }
        return Ok(());
    }

    // ---------- 研究模式：枚举目标子集 ----------
    if combinations {
        println!("╔══════════════════════════════════════════════════════════════════════════════╗");
        println!("║  研究模式：枚举目标子集 —— 这学期现实能同时推进哪几个目标？                     ║");
        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!(
            "学期 {} ~ {}｜可用整块 {} 个｜目标数 {}\n",
            term_start.format("%Y-%m-%d"),
            term_end.format("%Y-%m-%d"),
            slots.len(),
            cfg.goals.len()
        );
        let n = cfg.goals.len();
        let mut results: Vec<(u32, usize, planner::Feasibility)> = Vec::new();
        for mask in 1u32..(1 << n) {
            let subset: Vec<GoalState> = cfg
                .goals
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, g)| GoalState::new(g.clone()))
                .collect();
            let feas = feasibility(&subset, slots.len() as i64, term_end, term_start, cfg.block_minutes, 1);
            results.push((mask, subset.len(), feas));
        }
        // 可行的（缺口<=0）优先，其次目标多的，最后缺口小的
        results.sort_by(|a, b| {
            let ka = (a.2.gap().max(0), std::cmp::Reverse(a.1), a.2.gap());
            let kb = (b.2.gap().max(0), std::cmp::Reverse(b.1), b.2.gap());
            ka.cmp(&kb)
        });
        println!(
            "{:<34}{:<8}{:<10}{:<10}{:<10}{:<8}",
            "子集", "目标数", "硬需求", "总需求", "缺口", "可行"
        );
        println!("{}", "-".repeat(80));
        for (mask, cnt, feas) in &results {
            let names: Vec<&str> = cfg
                .goals
                .iter()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .map(|(_, g)| g.name.as_str())
                .collect();
            println!(
                "{:<34}{:<8}{:<10}{:<10}{:<10}{:<8}",
                names.join(" + "),
                cnt,
                feas.hard_need,
                feas.mandatory_need,
                feas.gap(),
                if feas.gap() <= 0 { "✓" } else { "✗" }
            );
        }
        let feasible: Vec<_> = results.iter().filter(|(_, _, f)| f.gap() <= 0).collect();
        println!("\n══ 结论 ══");
        if feasible.is_empty() {
            println!("  没有任何目标子集在这学期内可行 —— 连只做高数都放不下。");
        } else {
            let best = feasible
                .iter()
                .max_by_key(|(_, cnt, _)| *cnt)
                .expect("非空");
            let names: Vec<&str> = cfg
                .goals
                .iter()
                .enumerate()
                .filter(|(i, _)| best.0 & (1 << i) != 0)
                .map(|(_, g)| g.name.as_str())
                .collect();
            println!(
                "  最多能同时推进 {} 个目标：{}",
                best.1,
                names.join(" + ")
            );
            println!(
                "  （硬需求 {} 块，总需求 {} 块，可用 {} 块）",
                best.2.hard_need, best.2.mandatory_need, best.2.available_blocks
            );
        }
        return Ok(());
    }

    run_with_config(&cfg, &slots, &frag_by_day, &config_path.display().to_string(), term_start, term_end)
}

/// 跑一遍完整规划并打印报告（标准模式与 --plan 模式共用）。
fn run_with_config(
    cfg: &Config,
    slots: &[Slot],
    frag_by_day: &BTreeMap<DateTime<Utc>, i64>,
    config_label: &str,
    term_start: DateTime<Utc>,
    term_end: DateTime<Utc>,
) -> Result<()> {

        println!("╚══════════════════════════════════════════════════════════════════════════════╝");
        println!(
            "学期 {} ~ {}｜配置 {}",
            term_start.format("%Y-%m-%d"),
            term_end.format("%Y-%m-%d"),
            config_label
        );
        println!(
            "可用整块 {} 个（{} 分钟）｜碎片 {} 天（{} 分钟）",
            slots.len(),
            slots.len() as i64 * cfg.block_minutes,
            frag_by_day.len(),
            frag_by_day.values().sum::<i64>()
        );
        println!();

        // ---------- 碎片层：FSRS ----------
        let frags = simulate_fragments(cfg, term_start, term_end, frag_by_day);

        // ---------- 整块层 ----------
        let (by_week, goals, warnings) = run_plan(&cfg, &slots, term_start, term_end, None);

        for (wi, week_blocks) in by_week.iter().enumerate() {
            println!(
                "━━ 第{}周（{} ~ {}）· {} 块 ━━",
                wi + 1,
                (term_start + Duration::days((wi * 7) as i64)).format("%m-%d"),
                (term_start + Duration::days((wi * 7 + 6) as i64)).format("%m-%d"),
                week_blocks.len()
            );
            for b in week_blocks {
                let start_min = b.offset_minutes;
                let end_min = b.offset_minutes + b.duration_minutes;
                println!(
                    "   {} {}  {:<12} {:<18} {}  {}–{} ({:>3}min)  [{}]",
                    b.date.format("%m-%d"),
                    b.label,
                    b.goal_name,
                    b.milestone_name,
                    b.step_name,
                    fmt_min(start_min),
                    fmt_min(end_min),
                    b.duration_minutes,
                    b.kind.label()
                );
            }
        }

        // ---------- 期末状态 ----------
        println!("\n══ 期末状态 ══");
        for st in &goals {
            let total: i64 = st.spec.milestones.iter().map(|m| m.steps.iter().map(|x| x.minutes).sum::<i64>()).sum();
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

        // ---------- 冲突告警 ----------
        println!("\n══ 冲突告警 ══");
        if warnings.is_empty() {
            println!("  （无）");
        } else {
            for w in &warnings {
                println!("  ⚠ {w}");
            }
        }

        // ---------- 可行性诊断 ----------
        let feas = feasibility(&goals, slots.len() as i64, term_end, term_start, cfg.block_minutes, 1);
        println!("\n══ 可行性诊断 ══");
        println!("  学期内可用整块：{} 块", feas.available_blocks);
        println!("  硬需求合计（硬锚定+跟课）：{} 块", feas.mandatory_need);
        println!("  ├ 硬锚定（锚点在本学期内）：{} 块", feas.hard_need);
        println!("  ├ 跟课型（学校课程）：       {} 块", feas.course_need);
        println!("  └ 留给弹性目标的容量：       {} 块（弹性全量需求 {} 块）", feas.elastic_capacity, feas.elastic_need);
        let gap = feas.gap();
        if gap > 0 {
            println!(
                "  ⚠ 缺口 {gap} 块 —— 需求是可用量的 {:.1} 倍",
                feas.mandatory_need as f64 / feas.available_blocks as f64
            );
            if feas.hard_self_conflict() {
                println!(
                    "  ⚠⚠ 硬锚定目标自己就冲突：即使放弃全部弹性目标，仍缺 {} 块",
                    feas.hard_need - feas.available_blocks
                );
                println!("     → 至少有一个硬锚定目标必须改期（换考期 / 推迟报名）。");
            } else {
                println!("  → 硬需求可行，但弹性目标被完全挤出。");
            }
        } else {
            println!("  ✓ 目标组合可行");
        }

        // ---------- 取舍建议 ----------
        println!("\n══ 取舍建议（AI 该给人什么选项）══");
        let mut options: Vec<(String, f64, i64, f64)> = Vec::new();
        for st in &goals {
            let used = st.blocks_used as i64;
            let weekly = match st.spec.kind {
                GoalKind::Course => st.spec.weekly_blocks as i64,
                GoalKind::Pulse => 2,
                GoalKind::Daily => 0,
            };
            let releasable = if st.spec.kind == GoalKind::Daily {
                used
            } else {
                weekly * 6
            };
            if releasable > 0 {
                let eff = releasable as f64 / st.spec.weight;
                options.push((st.spec.name.clone(), st.spec.weight, releasable, eff));
            }
        }
        options.sort_by(|a, b| b.3.partial_cmp(&a.3).unwrap_or(std::cmp::Ordering::Equal));
        for (name, weight, rel, eff) in &options {
            println!("  · 暂缓「{name}」→ 释放约 {rel} 块（权重 {weight}，性价比 {eff:.2} 块/权重）");
        }

        // ---------- 碎片层 ----------
        println!("\n══ 碎片复习（FSRS v7）══");
        println!(
            "  {} 天｜总复习 {} 次｜期末留存 {:.1}%｜碎片总时长 {} 分钟",
            frags.days, frags.total_reviews, frags.retention * 100.0, frags.total_minutes
        );

    let _ = BlockKind::Flexible;
    Ok(())
}

/// 碎片复习模拟：真 FSRS 调度 + 每日时间预算
fn simulate_fragments(
    cfg: &Config,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    frag_by_day: &BTreeMap<DateTime<Utc>, i64>,
) -> FragResult {
    let fsrs = fsrs::FSRS::default();
    let mut rng = Rng(42);
    let mut cards: Vec<(Option<fsrs::MemoryState>, Option<DateTime<Utc>>, u32)> =
        vec![(None, None, 0); cfg.card_count];
    let mut total_reviews = 0u64;
    let mut recalls = 0u64;
    let mut attempts = 0u64;
    let mut total_minutes = 0i64;
    let mut days = 0usize;

    for c in cards.iter_mut() {
        let first = fsrs.next_states(None, cfg.desired_retention, 0).unwrap().good;
        c.0 = Some(first.memory);
        c.1 = Some(start);
        c.2 = 1;
    }

    let mut cur = start;
    while cur <= end {
        let minutes_today = frag_by_day
            .iter()
            .filter(|(d, _)| d.date_naive() == cur.date_naive())
            .map(|(_, m)| *m)
            .sum::<i64>();
        total_minutes += minutes_today;
        let budget = cfg.fragment_daily_budget_seconds.min(minutes_today as f64 * 60.0);
        let max_cards = (budget / cfg.seconds_per_card) as usize;

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
        due.sort_by(|&a, &b| {
            let ra = {
                let e = (cur - cards[a].1.unwrap()).num_seconds() as f32 / 86_400.0;
                fsrs.current_retrievability(cards[a].0.unwrap(), e)
            };
            let rb = {
                let e = (cur - cards[b].1.unwrap()).num_seconds() as f32 / 86_400.0;
                fsrs.current_retrievability(cards[b].0.unwrap(), e)
            };
            ra.partial_cmp(&rb)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for &i in due.iter().take(max_cards) {
            let e = (cur - cards[i].1.unwrap()).num_seconds() as f32 / 86_400.0;
            let r = fsrs.current_retrievability(cards[i].0.unwrap(), e);
            let recalled = rng.next_f64() < r as f64;
            let next = fsrs
                .next_states(cards[i].0, cfg.desired_retention, e.floor().max(0.0) as u32)
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

        days += 1;
        cur += Duration::days(1);
    }

    FragResult {
        days,
        total_reviews,
        retention: if attempts > 0 {
            recalls as f32 / attempts as f32
        } else {
            0.0
        },
        total_minutes,
    }
}
