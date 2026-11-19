//! 块 10 · 生命周期 —— Rust Book Ch10.3 (Validating References with Lifetimes)
//!
//! 跑法：`rustc --edition 2024 --test block10_lifetimes.rs -o /tmp/b10 && /tmp/b10`
//!
//! 本块产出：能读懂 `<'a>` 标注的含义，知道什么时候编译器能省略、什么时候必须写。

/// 最简单的情形：只有一个输入引用 → 编译器用「省略规则」自动补生命周期。
fn first_word(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

/// 两个输入引用，返回值可能来自其中任何一个 → 必须显式标注，
/// 且两个输入必须共享同一个生命周期 `'a`。
///
/// 这不是编译器要求的语法糖，而是**承诺**：返回的引用活得不会比任何一个入参更久。
fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {
    if a.len() >= b.len() { a } else { b }
}

/// 结构体持有引用 → 必须标生命周期。
/// `'a` 表示：Excerpt 的 part 借用的字符串，至少和 Excerpt 本身活得一样久。
#[derive(Debug, PartialEq)]
struct Excerpt<'a> {
    part: &'a str,
}

/// 结构体方法里的生命周期同样要带上。
impl<'a> Excerpt<'a> {
    /// 中文句号「。」和英文句点「.」都算句末 —— `split` 接受模式闭包。
    /// （只写 `split('.')` 切不开中文句号，这是个真实的坑。）
    fn first_sentence(&self) -> &str {
        self.part
            .split(|c| c == '.' || c == '。')
            .next()
            .unwrap_or(self.part)
    }
}

/// 三个生命周期参数：返回值只与第一个绑定，第二个独立。
/// 常见于「第一个参数是 self / 接收者，第二个是另一个入参」的签名。
fn longest_with_announcement<'a, 'b>(announcement: &'a str, a: &'b str, b: &'b str) -> &'a str {
    println!("注意：{announcement}");
    let _ = (a, b);
    announcement
}

/// 结构体的生命周期也可以加约束：要求 `'a` 不短于 `'b`。
fn holds_longer<'a, 'b: 'a>(long: &'a str, _short: &'b str) -> &'a str {
    long
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omission_rule_works() {
        assert_eq!(first_word("hello world"), "hello");
    }

    #[test]
    fn explicit_annotation_works() {
        assert_eq!(longest("abc", "abcdef"), "abcdef");
        assert_eq!(longest("abcdef", "abc"), "abcdef");
    }

    #[test]
    fn struct_holding_reference() {
        let text = String::from("Rust 是一门系统语言。它很可靠。");
        let excerpt = Excerpt {
            part: &text,
        };
        assert_eq!(excerpt.first_sentence(), "Rust 是一门系统语言");
        // text 仍然可用 —— 因为 excerpt 的生命周期没有超过 text
        assert!(text.contains("可靠"));
    }

    #[test]
    fn independent_lifetimes() {
        let msg = String::from("开始");
        let a = String::from("aa");
        let b = String::from("bb");
        assert_eq!(longest_with_announcement(&msg, &a, &b), "开始");
    }

    #[test]
    fn lifetime_bounds() {
        let long = String::from("long");
        let short = String::from("s");
        assert_eq!(holds_longer(&long, &short), "long");
    }
}
