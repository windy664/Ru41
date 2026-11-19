//! 块 6 · 错误处理 —— Rust Book Ch9 (Error Handling)
//!
//! 跑法：`rustc --edition 2024 --test block06_errors.rs -o /tmp/b06 && /tmp/b06`
//!
//! 本块产出：能用 Result + `?` 组合出可恢复的错误流，并区分 panic 与 Result 的使用场景。

use std::fmt;
use std::num::ParseIntError;

/// Ch9.2 可恢复错误用 Result 表示。
/// 这里用自定义错误类型（实际项目里通常用 thiserror，但本机拉不到 crate，所以手写）。
#[derive(Debug, PartialEq)]
enum MathError {
    DivisionByZero,
    Parse(ParseIntError),
}

/// 手写 Display + Error trait —— 这就是 thiserror 帮你自动生成的东西。
impl fmt::Display for MathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MathError::DivisionByZero => write!(f, "除数为零"),
            MathError::Parse(e) => write!(f, "解析失败: {e}"),
        }
    }
}

impl std::error::Error for MathError {}

impl From<ParseIntError> for MathError {
    fn from(e: ParseIntError) -> Self {
        MathError::Parse(e)
    }
}

/// Ch9.2 `?` 运算符：出错就提前返回，成功就取出值。
/// `?` 会自动做 From 转换，所以 ParseIntError 能变成 MathError。
fn parse_and_div(a: &str, b: &str) -> Result<i32, MathError> {
    let x: i32 = a.parse()?;
    let y: i32 = b.parse()?;
    if y == 0 {
        return Err(MathError::DivisionByZero);
    }
    Ok(x / y)
}

/// Ch9.1 panic：不可恢复错误。
/// `#[should_panic]` 是测试属性，声明这个函数会 panic。
#[should_panic(expected = "断言失败")]
fn panics_on_bad_input() {
    let v: Vec<i32> = vec![1, 2, 3];
    let bad = v[10]; // 下标越界 → panic
    println!("{bad}");
}

/// Ch9.3 什么时候用 panic、什么时候用 Result：
///   - panic：程序自身 bug（索引越界、除零）
///   - Result：用户输入可能不对，要给出可读的错误
fn parse_user_input(raw: &str) -> Result<i32, String> {
    raw.trim()
        .parse::<i32>()
        .map_err(|_| format!("「{raw}」不是合法整数"))
}

/// `map_err` 把底层错误换成人类可读的说明。
fn parse_user_input_nice(raw: &str) -> Result<i32, String> {
    raw.trim().parse::<i32>().map_err(|_| {
        format!("「{raw}」不是合法整数，请检查输入")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_mark_operator() {
        assert_eq!(parse_and_div("10", "2"), Ok(5));
        assert_eq!(parse_and_div("10", "0"), Err(MathError::DivisionByZero));
        assert!(matches!(
            parse_and_div("abc", "2"),
            Err(MathError::Parse(_))
        ));
    }

    #[test]
    fn panic_for_programmer_error() {
        // 用 catch_unwind 捕获，比 should_panic 属性更可控
        let result = std::panic::catch_unwind(panics_on_bad_input);
        assert!(result.is_err());
    }

    #[test]
    fn user_facing_errors_are_readable() {
        assert_eq!(parse_user_input(" 42 "), Ok(42));
        assert_eq!(
            parse_user_input("abc"),
            Err("「abc」不是合法整数".to_owned())
        );
        assert_eq!(
            parse_user_input_nice("abc"),
            Err("「abc」不是合法整数，请检查输入".to_owned())
        );
    }

    #[test]
    fn error_implements_display_and_error() {
        let e = MathError::DivisionByZero;
        assert_eq!(e.to_string(), "除数为零");
        // 证明它确实是标准 Error trait 的实现
        let boxed: Box<dyn std::error::Error> = Box::new(e);
        assert_eq!(boxed.to_string(), "除数为零");
    }
}
