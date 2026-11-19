//! 碎片池复习负载模拟：用真实 FSRS v7 算法算「Ru41 的碎片池能扛多少知识点」。
//!
//! Ru41 实测：课表里一周只有 90 分钟课间缝（7 段，平均 13 分钟/段），
//! 而整块（周一晚 + 周日上午）有 13 个 2h 块。
//!
//! 架构前提（对应 portfolio.md 的分层）：
//!   - **整块**用来学新知识（第一次学要连续时间）
//!   - **碎片**只用来复习已学的卡（FSRS 调度）
//!
//! 对比两种复习取舍策略在**同一时间预算**下的表现：
//!   A Naive：到期卡按创建顺序过（预算不够时，先到先得）
//!   B FSRS ：到期卡按"最接近遗忘"优先（预算不够时，保住最该记住的）
//!
//! 跑法：cargo run --release

use chrono::{DateTime, Duration, TimeZone, Utc};
use fsrs::{FSRS, MemoryState};

#[derive(Clone, Copy, PartialEq)]
enum Strategy {
    /// 到期卡按 id 顺序过 —— 预算不够时先到先得
    Naive,
    /// 到期卡按「最接近遗忘」优先 —— 预算不够时保住最该记住的
    Fsrs,
}

struct Card {
    id: usize,
    due: DateTime<Utc>,
    memory: Option<MemoryState>,
    last_review: Option<DateTime<Utc>>,
    reps: u32,
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

#[derive(Default)]
struct Report {
    total_reviews: u64,
    recalls: u64,
    attempts: u64,
    peak_backlog: usize,
    /// 学期末「掌握」的卡：复习过 ≥2 次且当前留存率 ≥ 0.8
    mastered: usize,
    /// 学期末复习次数为 0 的卡（完全没被覆盖）
    never_reviewed: usize,
    /// 到期末还没到期的卡（从没复习过的新卡）
    untouched: usize,
    weekly: Vec<(usize, u64, usize)>,
}

fn simulate(
    fsrs: &FSRS,
    strategy: Strategy,
    n_cards: usize,
    days: usize,
    daily_review_seconds: f64,
    seconds_per_card: f64,
    new_cards_per_week: usize,
    desired_retention: f32,
    seed: u64,
) -> Report {
    let mut rng = Rng(seed);
    let start = Utc.with_ymd_and_hms(2026, 11, 23, 0, 0, 0).unwrap();
    // 新卡默认「还没学」，due 设为无穷远 —— 只有被整块引入后才进入复习队列
    let never = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap();
    let mut cards: Vec<Card> = (0..n_cards)
        .map(|id| Card {
            id,
            due: never,
            memory: None,
            last_review: None,
            reps: 0,
        })
        .collect();

    let max_per_day = (daily_review_seconds / seconds_per_card).floor() as usize;
    let mut rep = Report::default();
    let mut week_reviews = 0u64;

    for day in 0..days {
        let now = start + Duration::days(day as i64);

        // 整块时间引入新卡：每周 new_cards_per_week 张，周一引入
        if day % 7 == 0 {
            let batch = (day / 7) * new_cards_per_week;
            for id in batch..(batch + new_cards_per_week).min(n_cards) {
                // 首次学习：在整块时间里主动学一遍 → 建立初始记忆状态（S0/D），
                // 并排定第一次复习。
                let first = fsrs
                    .next_states(None, desired_retention, 0)
                    .expect("next_states(new)")
                    .good;
                cards[id].memory = Some(first.memory);
                cards[id].last_review = Some(now);
                cards[id].due = now
                    + Duration::seconds((first.interval as f64 * 86_400.0).round() as i64);
                cards[id].reps = 1;
            }
        }

        // 到期卡
        let mut due: Vec<usize> = (0..n_cards)
            .filter(|&i| cards[i].due <= now && cards[i].memory.is_some())
            .collect();
        rep.peak_backlog = rep.peak_backlog.max(due.len());

        match strategy {
            Strategy::Naive => due.sort_by_key(|&i| cards[i].id),
            Strategy::Fsrs => {
                // 按当前留存率升序：最接近遗忘的先复习
                due.sort_by(|&a, &b| {
                    let ra = cards[a]
                        .memory
                        .map(|m| fsrs.current_retrievability(m, elapsed(&cards[a], now)))
                        .unwrap_or(1.0);
                    let rb = cards[b]
                        .memory
                        .map(|m| fsrs.current_retrievability(m, elapsed(&cards[b], now)))
                        .unwrap_or(1.0);
                    ra.partial_cmp(&rb).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        }

        let take = due.len().min(max_per_day);
        for &i in due.iter().take(take) {
            let card = &mut cards[i];
            let elapsed_days = elapsed(card, now);
            let r = fsrs.current_retrievability(card.memory.unwrap(), elapsed_days);
            let recalled = rng.next_f64() < r as f64;

            let next = fsrs
                .next_states(card.memory, desired_retention, elapsed_days.floor().max(0.0) as u32)
                .expect("next_states");
            let ns = if recalled { next.good } else { next.again };

            card.memory = Some(ns.memory);
            card.last_review = Some(now);
            let interval_secs = (ns.interval as f64 * 86_400.0).round() as i64;
            card.due = now + Duration::seconds(interval_secs.max(60));
            card.reps += 1;

            rep.total_reviews += 1;
            rep.attempts += 1;
            week_reviews += 1;
            if recalled {
                rep.recalls += 1;
            }
        }

        if day % 7 == 6 {
            rep.weekly.push((day / 7 + 1, week_reviews, due.len()));
            week_reviews = 0;
        }
    }

    // 期末统计
    for c in &cards {
        if c.reps == 0 {
            rep.untouched += 1;
            continue;
        }
        if c.reps >= 2 {
            let r = fsrs.current_retrievability(c.memory.unwrap(), elapsed(c, start + Duration::days(days as i64)));
            if r >= 0.8 {
                rep.mastered += 1;
            }
        }
    }
    rep.never_reviewed = rep.untouched;
    rep
}

fn elapsed(card: &Card, now: DateTime<Utc>) -> f32 {
    match card.last_review {
        Some(lr) => (now - lr).num_seconds() as f32 / 86_400.0,
        None => 0.0,
    }
}

/// 退步信号统计：期末有多少卡「需要整块重学」、多少卡「已经很稳」、平均难度。
fn weakness_signals(
    fsrs: &FSRS,
    n_cards: usize,
    days: usize,
    new_cards_per_week: usize,
    desired_retention: f32,
    seed: u64,
) -> (usize, usize, f32) {
    let mut rng = Rng(seed);
    let start = Utc.with_ymd_and_hms(2026, 11, 23, 0, 0, 0).unwrap();
    let mut cards: Vec<(Option<MemoryState>, Option<DateTime<Utc>>, u32, u32)> =
        vec![(None, None, 0, 0); n_cards];
    let mut weak = 0usize;
    let mut strong = 0usize;
    let mut d_sum = 0.0f32;
    let mut d_count = 0usize;

    for day in 0..days {
        let now = start + Duration::days(day as i64);
        if day % 7 == 0 {
            let batch = (day / 7) * new_cards_per_week;
            for id in batch..(batch + new_cards_per_week).min(n_cards) {
                let first = fsrs.next_states(None, desired_retention, 0).unwrap().good;
                cards[id].0 = Some(first.memory);
                cards[id].1 = Some(now);
                cards[id].3 = 1; // reps
            }
        }
        let mut due: Vec<usize> = (0..n_cards)
            .filter(|&i| {
                cards[i].0.is_some()
                    && cards[i]
                        .1
                        .map(|lr| {
                            let due = lr
                                + Duration::seconds(
                                    (fsrs.next_states(cards[i].0, desired_retention, 0).unwrap().good.interval
                                        as f64
                                        * 86_400.0)
                                        .round() as i64,
                                );
                            due <= now
                        })
                        .unwrap_or(false)
            })
            .collect();
        // 简化：按 due 时间近似 —— 这里直接用上次复习后经过的间隔判断到期
        due.retain(|&i| {
            let lr = cards[i].1.unwrap();
            let e = (now - lr).num_seconds() as f32 / 86_400.0;
            fsrs.current_retrievability(cards[i].0.unwrap(), e) < 0.9
        });
        due.sort_by_key(|&i| cards[i].1.unwrap());

        for &i in due.iter().take(78) {
            let elapsed_days = (now - cards[i].1.unwrap()).num_seconds() as f32 / 86_400.0;
            let r = fsrs.current_retrievability(cards[i].0.unwrap(), elapsed_days);
            let recalled = rng.next_f64() < r as f64;
            let next = fsrs
                .next_states(cards[i].0, desired_retention, elapsed_days.floor().max(0.0) as u32)
                .unwrap();
            let ns = if recalled { next.good } else { next.again };
            cards[i].0 = Some(ns.memory);
            cards[i].1 = Some(now);
            cards[i].3 += 1;
            if !recalled {
                cards[i].2 += 1; // lapses
            }
        }
    }

    for (mem, _, reps, lapses) in &cards {
        let Some(m) = mem else { continue };
        d_sum += m.difficulty;
        d_count += 1;
        if m.difficulty >= 7.0 || *lapses >= 3 {
            weak += 1;
        }
        if m.difficulty <= 3.0 && *reps >= 3 {
            strong += 1;
        }
    }
    (weak, strong, if d_count > 0 { d_sum / d_count as f32 } else { 0.0 })
}

fn print_report(name: &str, r: &Report) {
    let retention = if r.attempts > 0 {
        r.recalls as f64 / r.attempts as f64
    } else {
        0.0
    };
    println!("\n=== {name} ===");
    println!(
        "总复习 {} 次｜留存率 {:.1}%｜掌握(R≥0.8 且复习≥2次) {} 张｜积压峰值 {}",
        r.total_reviews,
        retention * 100.0,
        r.mastered,
        r.peak_backlog
    );
    print!("每周(复习数/积压)：");
    for (w, n, backlog) in &r.weekly {
        print!("W{w}:{n}/{backlog} ");
    }
    println!();
}

fn main() {
    let fsrs = FSRS::default();
    let n_cards = 300usize;
    let days = 112usize; // 16 周
    let seconds_per_card = 10.0f64;
    let new_cards_per_week = 20usize; // 整块时间每周学 20 个新知识点
    let desired_retention = 0.90f32;

    println!("FSRS v7 碎片池复习负载模拟（Ru41 真实课表口径）");
    println!("─────────────────────────────────────────────────────────");
    println!("知识库 {n_cards} 张卡｜{days} 天（16 周）｜每张 {seconds_per_card:.0} 秒");
    println!("整块引入新卡：每周 {new_cards_per_week} 张（周一）");
    println!("碎片复习预算：见下表（每天 13 分钟 = 90min/周 ÷ 7）");
    println!("目标留存率 {:.0}%", desired_retention * 100.0);

    println!("\n【场景一】碎片预算充足（13 分钟/天）");
    let a = simulate(&fsrs, Strategy::Naive, n_cards, days, 13.0 * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
    let b = simulate(&fsrs, Strategy::Fsrs, n_cards, days, 13.0 * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
    print_report("A Naive（按创建顺序）", &a);
    print_report("B FSRS（按最接近遗忘优先）", &b);
    println!("→ 预算充足时两者等价：瓶颈是整块引入新卡的速度，不是碎片");

    println!("\n【场景二】碎片预算吃紧（3 分钟/天 ≈ 21min/周）");
    let a2 = simulate(&fsrs, Strategy::Naive, n_cards, days, 3.0 * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
    let b2 = simulate(&fsrs, Strategy::Fsrs, n_cards, days, 3.0 * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
    print_report("A Naive（按创建顺序）", &a2);
    print_report("B FSRS（按最接近遗忘优先）", &b2);
    println!(
        "→ 预算吃紧时：掌握 {} 张 vs {} 张（FSRS 多保住 {} 张）",
        a2.mastered, b2.mastered, b2.mastered as i64 - a2.mastered as i64
    );

    println!("\n【场景三】预算扫掠（每天碎片分钟数 → 学期末掌握卡数）");
    println!("{:<12}{:<14}{:<14}{:<10}", "每天分钟", "Naive 掌握", "FSRS 掌握", "差距");
    for mins in [2.0f64, 3.0, 5.0, 8.0, 13.0] {
        let na = simulate(&fsrs, Strategy::Naive, n_cards, days, mins * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
        let fb = simulate(&fsrs, Strategy::Fsrs, n_cards, days, mins * 60.0, seconds_per_card, new_cards_per_week, desired_retention, 42);
        println!(
            "{:<12.0}{:<14}{:<14}{:<10}",
            mins,
            na.mastered,
            fb.mastered,
            fb.mastered as i64 - na.mastered as i64
        );
    }

    println!("\n【场景四】整块引入速度的影响（碎片固定 13 分钟/天）");
    println!("{:<14}{:<14}{:<14}", "每周新卡", "Naive 掌握", "FSRS 掌握");
    for per_week in [10usize, 20, 30, 50] {
        let na = simulate(&fsrs, Strategy::Naive, n_cards, days, 13.0 * 60.0, seconds_per_card, per_week, desired_retention, 42);
        let fb = simulate(&fsrs, Strategy::Fsrs, n_cards, days, 13.0 * 60.0, seconds_per_card, per_week, desired_retention, 42);
        println!("{:<14}{:<14}{:<14}", per_week, na.mastered, fb.mastered);
    }

    println!("\n【场景五】目标留存率的代价（300 卡 / 13min 每天）");
    println!("{:<14}{:<16}{:<18}{:<12}", "目标留存", "总复习次数", "每天需碎片", "积压峰值");
    for ret in [0.85f32, 0.90, 0.95, 0.98] {
        let r = simulate(&fsrs, Strategy::Fsrs, n_cards, days, 13.0 * 60.0, seconds_per_card, new_cards_per_week, ret, 42);
        let per_day = r.total_reviews as f64 * seconds_per_card / days as f64 / 60.0;
        println!(
            "{:<14.0}{:<16}{:<18.1}{:<12}",
            ret * 100.0,
            r.total_reviews,
            per_day,
            r.peak_backlog
        );
    }

    println!("\n【场景六】退步信号：FSRS 能告诉「我在哪些知识点上退步了」");
    let (weak, strong, avg_d) = weakness_signals(&fsrs, n_cards, days, new_cards_per_week, desired_retention, 42);
    println!("  学期末「需要用整块重新学」的卡（D≥7 或 反复 Again）：{weak} 张");
    println!("  学期末「很稳」的卡（D≤3 且复习≥3 次）：{strong} 张");
    println!("  平均难度 D：{avg_d:.2}（1=很容易，10=很难）");
    println!("  → 这些「退步信号」可以直接驱动整块时间的重新分配：");
    println!("    某个知识点反复 Again → 把它排进整块，而不是靠碎片再刷一次");
}
