//! 块 5 · 枚举与模式匹配 —— Rust Book Ch6 (Enums and Pattern Matching)
//!
//! 跑法：`rustc --edition 2024 --test block05_enums.rs -o /tmp/b05 && /tmp/b05`
//!
//! 本块产出：能用 enum + Option 建模「可能没有值」，用 match / if let / let-else 处理。

/// Ch6.1 定义枚举。
#[derive(Debug, Clone, PartialEq)]
enum Direction {
    North,
    East,
    South,
    West,
}

/// Ch6.2 match 必须穷尽；带数据的变体要解构。
fn turn(d: &Direction) -> &'static str {
    match d {
        Direction::North => "北",
        Direction::East => "东",
        Direction::South => "南",
        Direction::West => "西",
    }
}

/// Ch6.1 枚举可以携带数据（像函数调用一样构造）。
#[derive(Debug, PartialEq)]
enum Message {
    Quit,
    Move { x: i32, y: i32 },
    Write(String),
    ChangeColor(i32, i32, i32),
}

fn describe(msg: &Message) -> String {
    match msg {
        Message::Quit => "退出".to_owned(),
        Message::Move { x, y } => format!("移动到 ({x}, {y})"),
        Message::Write(text) => format!("写入 {text}"),
        Message::ChangeColor(r, g, b) => format!("改色 ({r},{g},{b})"),
    }
}

/// Ch6.1 Option<T> 是标准库定义的枚举：`Some(T)` / `None`。
/// 没有「空指针」，「可能没有值」是一等公民。
fn first_char_upper(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let first = chars.next()?;
    Some(first.to_ascii_uppercase())
}

/// Ch6.3 if let：只关心一种模式时用。
fn describe_opt(v: Option<i32>) -> String {
    if let Some(n) = v {
        format!("有值 {n}")
    } else {
        "无值".to_owned()
    }
}

/// Ch6.3 let...else：匹配不上就提前返回。
fn require_name(v: &Option<String>) -> &str {
    let Some(name) = v else {
        return "(匿名)";
    };
    name
}

/// Ch6.2 match 可以带守卫（guard）。
fn classify(n: i32) -> &'static str {
    match n {
        x if x % 15 == 0 => "FizzBuzz",
        x if x % 3 == 0 => "Fizz",
        x if x % 5 == 0 => "Buzz",
        _ => "普通",
    }
}

/// `@` 绑定：既检查范围又取出值。
fn describe_age(age: u32) -> String {
    match age {
        0 => "刚出生".to_owned(),
        n @ 1..=12 => format!("儿童 {n}"),
        n @ 13..=17 => format!("青少年 {n}"),
        n if n >= 18 => format!("成年 {n}"),
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_and_match() {
        assert_eq!(turn(&Direction::North), "北");
        assert_eq!(turn(&Direction::West), "西");
    }

    #[test]
    fn enum_with_data() {
        assert_eq!(describe(&Message::Move { x: 1, y: 2 }), "移动到 (1, 2)");
        assert_eq!(describe(&Message::Write("hi".into())), "写入 hi");
        assert_eq!(describe(&Message::ChangeColor(1, 2, 3)), "改色 (1,2,3)");
    }

    #[test]
    fn option_flow() {
        assert_eq!(first_char_upper("rust"), Some('R'));
        assert_eq!(first_char_upper(""), None);
        assert_eq!(describe_opt(Some(3)), "有值 3");
        assert_eq!(describe_opt(None), "无值");
        assert_eq!(require_name(&Some("Torin".to_owned())), "Torin");
        assert_eq!(require_name(&None), "(匿名)");
    }

    #[test]
    fn guards_and_bindings() {
        assert_eq!(classify(15), "FizzBuzz");
        assert_eq!(classify(9), "Fizz");
        assert_eq!(classify(7), "普通");
        assert_eq!(describe_age(10), "儿童 10");
        assert_eq!(describe_age(20), "成年 20");
    }
}
