//! 块 1 · 环境与基本语法 —— Rust Book Ch1 (Getting Started) + Ch3 (Common Programming Concepts)
//!
//! 跑法：`rustc --edition 2024 --test block01_basics.rs -o /tmp/b01 && /tmp/b01`
//!
//! 本块产出：能 `cargo new` 一个项目并让 `cargo test` 跑起来；手写这一份不依赖任何 crate。

/// Ch3.1 变量默认不可变。`let mut` 才可修改。
/// Ch3.1 遮蔽（shadowing）：同名新变量，可以换类型。
fn shadowing_demo() -> i32 {
    let x = 5;
    let x = x + 1; // 6
    let x = format!("{x}"); // 同名，类型变成 String
    x.len() as i32 // "6" 长度 1
}

/// Ch3.2 数据类型：整数默认 i32；`as` 做显式转换。
fn safe_div(a: i32, b: i32) -> Option<i32> {
    if b == 0 {
        None
    } else {
        Some(a / b)
    }
}

/// Ch3.3 函数：参数与返回值都要标类型。
fn is_even(n: i32) -> bool {
    n % 2 == 0
}

/// Ch3.5 `if` 是表达式，可以直接返回值。
fn abs(n: i32) -> i32 {
    if n < 0 { -n } else { n }
}

/// Ch3.5 `loop` 可以带返回值（`break value`）。
fn sum_until_over(n: u32) -> u32 {
    let mut total = 0u32;
    loop {
        total += n;
        if total > 100 {
            break total;
        }
    }
}

/// Ch3.5 `match` 必须穷尽所有可能。
fn grade(score: u32) -> &'static str {
    match score {
        90..=100 => "A",
        80..=89 => "B",
        60..=79 => "C",
        _ => "F",
    }
}

/// Ch3.5 `while` 循环 + 累加器。
fn factorial(n: u64) -> u64 {
    let mut acc = 1u64;
    let mut i = 2u64;
    while i <= n {
        acc *= i;
        i += 1;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadowing_can_change_type() {
        assert_eq!(shadowing_demo(), 1);
    }

    #[test]
    fn division_by_zero_is_none_not_panic() {
        assert_eq!(safe_div(10, 2), Some(5));
        assert_eq!(safe_div(10, 0), None);
    }

    #[test]
    fn control_flow_forms() {
        assert!(is_even(4));
        assert!(!is_even(5));
        assert_eq!(abs(-7), 7);
        assert_eq!(sum_until_over(30), 120);
        assert_eq!(grade(95), "A");
        assert_eq!(grade(0), "F");
        assert_eq!(factorial(5), 120);
    }
}
