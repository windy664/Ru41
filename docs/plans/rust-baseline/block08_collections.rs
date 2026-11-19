//! 块 8 · 常用集合 —— Rust Book Ch8 (Common Collections)
//!
//! 跑法：`rustc --edition 2024 --test block08_collections.rs -o /tmp/b08 && /tmp/b08`
//!
//! 本块产出：能用 Vec / String / HashMap 写出「统计词频」这类真实小工具。

use std::collections::HashMap;

/// Ch8.1 Vec：连续内存的同类元素集合。
fn sum_of_pushes() -> i32 {
    let mut v: Vec<i32> = Vec::new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.iter().sum()
}

/// Ch8.1 越界会 panic（和数组一样），所以安全起见用 `get`。
fn safe_first(v: &[i32]) -> Option<i32> {
    v.get(0).copied()
}

/// Ch8.2 String：UTF-8 可增长文本。
fn shout(text: &str) -> String {
    let mut s = String::from(text);
    s.push_str("!");
    s.to_uppercase()
}

/// Ch8.2 按空白切分并收集成 Vec<&str>。
fn words(text: &str) -> Vec<&str> {
    text.split_whitespace().collect()
}

/// Ch8.3 HashMap：键值对。entry API 是计数惯用法。
fn word_counts(text: &str) -> HashMap<&str, u32> {
    let mut counts = HashMap::new();
    for w in text.split_whitespace() {
        // entry 返回「键对应的可变引用」，不存在则插入默认值
        let counter = counts.entry(w).or_insert(0);
        *counter += 1;
    }
    counts
}

/// Ch8.3 也可以用 `*counts.entry(w).or_insert(0) += 1;` 一步写完。
fn word_counts_1liner(text: &str) -> HashMap<&str, u32> {
    let mut counts = HashMap::new();
    for w in text.split_whitespace() {
        *counts.entry(w).or_insert(0) += 1;
    }
    counts
}

/// 实际小工具：统计词频并按次数排序输出。
fn top_words(text: &str, n: usize) -> Vec<(String, u32)> {
    let mut items: Vec<(String, u32)> = word_counts(text)
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
    items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    items.truncate(n);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_basics() {
        assert_eq!(sum_of_pushes(), 6);
        assert_eq!(safe_first(&[7, 8]), Some(7));
        assert_eq!(safe_first(&[]), None);
    }

    #[test]
    fn string_basics() {
        assert_eq!(shout("hi"), "HI!");
        assert_eq!(words("a b  c"), vec!["a", "b", "c"]);
    }

    #[test]
    fn hashmap_counting() {
        let c = word_counts("the quick the fox the");
        assert_eq!(c["the"], 3);
        assert_eq!(c["fox"], 1);
        assert_eq!(word_counts_1liner("the quick the fox the"), c);
    }

    #[test]
    fn top_words_ranks_correctly() {
        // "the" 出现 4 次排第一；其余都是 1 次，按字母序取前两个 → dog, end
        let top = top_words("the quick the fox the lazy dog the end", 2);
        assert_eq!(top[0], ("the".to_owned(), 4));
        assert_eq!(top[1], ("dog".to_owned(), 1));
    }
}
