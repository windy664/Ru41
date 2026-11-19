//! 块 11 · 动手项目一：猜数字游戏 —— Rust Book Ch2 (Programming a Guessing Game)
//!
//! 跑法：`rustc --edition 2024 --test block11_guessing_game.rs -o /tmp/b11 && /tmp/b11`
//!
//! 本块产出：一个能玩的猜数字游戏，并理解「依赖注入」——
//! 把随机源做成参数，测试就能控制它。这是 Rust 里做可测设计的关键手法。
//!
//! 注：官方教程用 `rand` crate，本机拉不到外部 crate，所以用自带的确定性 LCG；
//! 原理与 rand 的 `thread_rng` 封装一致。

/// 确定性伪随机数发生器（LCG）。生产里应该用 rand crate。
struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // 参数取自 Numerical Recipes 的常用常数
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    /// 生成 [low, high] 闭区间内的整数。
    fn range(&mut self, low: u64, high: u64) -> u64 {
        assert!(low <= high);
        low + self.next_u64() % (high - low + 1)
    }
}

/// 一次猜测的结果。
#[derive(Debug, PartialEq)]
enum GuessResult {
    TooSmall,
    TooBig,
    Correct,
    InvalidInput,
}

/// Ch2 的核心循环。`guess` 是闭区间，返回实际猜中的数字。
fn play(target: u64, mut guess: impl FnMut(&str) -> String) -> u64 {
    println!("猜一个 1..=100 的数（输入 quit 退出）");
    loop {
        let input = guess("请输入：");
        let input = input.trim();

        if input.eq_ignore_ascii_case("quit") {
            println!("退出");
            return target;
        }

        // Ch2：用 match 处理非法输入，而不是让程序崩掉
        let n: u64 = match input.parse() {
            Ok(n) if (1..=100).contains(&n) => n,
            Ok(_) => {
                println!("请输入 1 到 100 之间的整数");
                continue;
            }
            Err(_) => {
                println!("这不是一个数字");
                continue;
            }
        };

        // Ch2：把比较结果抽成枚举，match 处理
        let result = match n.cmp(&target) {
            std::cmp::Ordering::Less => GuessResult::TooSmall,
            std::cmp::Ordering::Greater => GuessResult::TooBig,
            std::cmp::Ordering::Equal => GuessResult::Correct,
        };

        match result {
            GuessResult::TooSmall => println!("太小了"),
            GuessResult::TooBig => println!("太大了"),
            GuessResult::Correct => {
                println!("猜对了！答案是 {target}");
                return target;
            }
            GuessResult::InvalidInput => unreachable!(),
        }
    }
}

/// 供测试用：可编程的输入源。
struct ScriptedInput {
    lines: Vec<String>,
    idx: usize,
}

impl ScriptedInput {
    fn new(lines: &[&str]) -> Self {
        Self {
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            idx: 0,
        }
    }
}

/// 把「读 stdin」换成「读脚本」—— 这就是依赖注入。
fn play_with_script(target: u64, script: &[&str]) -> u64 {
    let mut input = ScriptedInput::new(script);
    play(target, |_| {
        let line = input.lines.get(input.idx).cloned().unwrap_or_default();
        input.idx += 1;
        line
    })
}

fn main() {
    let mut rng = Lcg::new(0x2545F491_4F6CDD1D);
    let target = rng.range(1, 100);
    play(target, |prompt| {
        use std::io::Write;
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap_or(0);
        line
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcg_is_deterministic() {
        let mut a = Lcg::new(42);
        let mut b = Lcg::new(42);
        assert_eq!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn range_respects_bounds() {
        let mut rng = Lcg::new(7);
        for _ in 0..100 {
            let v = rng.range(1, 100);
            assert!((1..=100).contains(&v));
        }
    }

    #[test]
    fn wins_on_correct_guess() {
        assert_eq!(play_with_script(50, &["50"]), 50);
    }

    #[test]
    fn handles_invalid_input_then_wins() {
        // 非法输入不该消耗机会，也不该崩
        assert_eq!(play_with_script(42, &["abc", "999", "42"]), 42);
    }

    #[test]
    fn quit_returns_without_crashing() {
        assert_eq!(play_with_script(50, &["quit"]), 50);
    }
}
