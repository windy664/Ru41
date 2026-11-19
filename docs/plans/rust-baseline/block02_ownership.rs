//! 块 2 · 所有权（上）—— Rust Book Ch4.1 (What is Ownership?) + Ch4.2 (References and Borrowing)
//!
//! 跑法：`rustc --edition 2024 --test block02_ownership.rs -o /tmp/b02 && /tmp/b02`
//!
//! 本块产出：能用自己的话解释「移动语义」，并说清借用规则的三条限制。

/// Ch4.1 所有权三条规则：
///   1. 每个值有且只有一个所有者
///   2. 所有者离开作用域时值被丢弃
///   3. 赋值/传参/返回值 = 移动（move），原变量失效
///
/// 这个函数能编译，说明 `String` 是移动语义（堆数据不能浅拷贝）。
fn takes_ownership(s: String) -> usize {
    s.len()
}

/// Ch4.1 但 `i32` 是 `Copy`：赋值是复制，原变量仍可用。
fn copies_instead_of_moves(x: i32) -> (i32, i32) {
    let y = x; // 复制，不是移动
    (x, y) // x 依然可用
}

/// Ch4.2 不可变借用：可以同时存在多个 &T。
fn count_words(text: &String) -> usize {
    text.split_whitespace().count()
}

/// Ch4.2 可变借用：同一时刻只能有一个 &mut T。
fn push_twice(v: &mut Vec<String>) {
    v.push("a".to_owned());
    v.push("b".to_owned());
}

/// Ch4.2 借用与所有权结合：把 &str 借来用，不夺走所有权。
fn first_word(text: &String) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_invalidates_source() {
        let s = String::from("hello rust");
        let len = takes_ownership(s);
        assert_eq!(len, 10);
        // s 在这里已被移动，不能再用 —— 这正是所有权规则 3。
    }

    #[test]
    fn copy_types_stay_usable() {
        let (a, b) = copies_instead_of_moves(7);
        assert_eq!((a, b), (7, 7));
    }

    #[test]
    fn borrowing_does_not_take_ownership() {
        let text = String::from("one two three");
        assert_eq!(count_words(&text), 3);
        assert_eq!(first_word(&text), "one");
        assert_eq!(text, "one two three"); // 仍然可用
    }

    #[test]
    fn mutable_borrow_allows_mutation() {
        let mut v: Vec<String> = Vec::new();
        push_twice(&mut v);
        assert_eq!(v, vec!["a".to_owned(), "b".to_owned()]);
    }
}
