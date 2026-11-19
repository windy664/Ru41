//! 块 4 · 借用检查器实操 —— Rust Book Ch4.2 的三类借用错误
//!
//! 跑法：`rustc --edition 2024 --test block04_borrow_checker.rs -o /tmp/b04 && /tmp/b04`
//!
//! 本块产出：能读懂并修复下面三类编译错误。
//! 每个错误案例都以注释形式保留 —— 取消注释即可复现编译失败，观察 rustc 的报错。
//! 这正是「让编译器教你」的训练方式。

/// ✅ 正确：只读借用存活期间，变量不可修改。
fn ok_immutable_borrow_then_read() -> usize {
    let text = String::from("hello");
    let len = text.len(); // 借用结束
    text.len() + len as usize // 仍可读
}

/// ✅ 正确：可变借用独占，先借用再改。
fn ok_mutable_borrow() -> usize {
    let mut v = vec![1, 2, 3];
    let first = v.first().copied().unwrap_or(0); // 借用结束
    v.push(4); // 现在可以改了
    v.len() + first as usize
}

/// ❌ 错误 1（Ch4.2）：借用一个值之后再修改它。
/// ```compile_fail
/// fn err_borrow_then_mutate() {
///     let mut s = String::from("hi");
///     let r = &s;
///     s.push('!');
///     println!("{r}");
/// }
/// ```
/// 报错：`cannot borrow `s` as mutable because it is also borrowed as immutable`

/// ❌ 错误 2（Ch4.2）：借用一个值之后再移动它。
/// ```compile_fail
/// fn err_borrow_then_move() {
///     let s = String::from("hi");
///     let r = &s;
///     let t = s;      // 移动
///     println!("{r} {t}");
/// }
/// ```
/// 报错：`borrow of moved value: `s``

/// ❌ 错误 3（Ch4.2）：借用一个值之后再销毁它。
/// ```compile_fail
/// fn err_borrow_then_drop() {
///     let s = String::from("hi");
///     let r = &s;
///     drop(s);
///     println!("{r}");
/// }
/// ```
/// 报错：`borrow of moved value: `s``

/// ✅ 修法一：缩短借用作用域（让引用用完就结束）。
fn fix_by_shortening_borrow() -> usize {
    let mut v = vec![1, 2, 3];
    let len = v.len(); // 借用到此为止
    v.push(4);
    v.len() + len // 4 + 3 = 7
}

/// ✅ 修法二：把「借用」改成「克隆」——牺牲性能换取灵活。
fn fix_by_cloning() -> usize {
    let s = String::from("hi");
    let r = s.clone(); // 克隆而不是借用
    let t = s; // 现在可以移动
    r.len() + t.len()
}

/// ✅ 修法三：作用域（block）——借用不能活得比所有者久。
fn fix_by_scoping() -> usize {
    let len;
    {
        let s = String::from("hi");
        len = s.len();
    } // s 在这里被销毁
    len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_patterns_compile() {
        assert_eq!(ok_immutable_borrow_then_read(), 10);
        assert_eq!(ok_mutable_borrow(), 5);
    }

    #[test]
    fn three_fixes() {
        assert_eq!(fix_by_shortening_borrow(), 7);
        assert_eq!(fix_by_cloning(), 4);
        assert_eq!(fix_by_scoping(), 2);
    }
}
