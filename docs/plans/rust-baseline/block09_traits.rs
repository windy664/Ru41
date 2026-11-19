//! 块 9 · 泛型与 Trait —— Rust Book Ch10.1 (Generic Data Types) + Ch10.2 (Defining Shared Behavior with Traits)
//!
//! 跑法：`rustc --edition 2024 --test block09_traits.rs -o /tmp/b09 && /tmp/b09`
//!
//! 本块产出：能定义 trait（带默认实现）、写泛型函数、用 trait bound 约束类型。

/// Ch10.1 泛型函数：`T: PartialOrd` 是 trait bound。
fn largest<T: PartialOrd>(list: &[T]) -> Option<&T> {
    let mut iter = list.iter();
    let first = iter.next()?;
    let mut best = first;
    for item in iter {
        if item > best {
            best = item;
        }
    }
    Some(best)
}

/// Ch10.2 定义 trait。方法可以有默认实现。
trait Summary {
    fn summarize(&self) -> String;
    /// 默认实现：实现者可以不写。
    fn source(&self) -> &str {
        "匿名"
    }
}

struct Article {
    title: String,
    author: String,
}

struct Tweet {
    user: String,
}

/// Ch10.2 为类型实现 trait。
impl Summary for Article {
    fn summarize(&self) -> String {
        format!("《{}》— {}", self.title, self.author)
    }
    fn source(&self) -> &str {
        &self.author
    }
}

impl Summary for Tweet {
    fn summarize(&self) -> String {
        format!("@{} 的推文", self.user)
    }
    // source 用默认实现
}

/// Ch10.2 `impl Trait` 语法：等价于泛型 + bound，但更简洁。
fn notify(item: &impl Summary) -> String {
    item.summarize()
}

/// 泛型 + 显式 bound 的等价写法。
fn notify_generic<T: Summary>(item: &T) -> String {
    item.summarize()
}

/// Ch10.1 trait 可以有多个 bound，用 `+` 连接。
fn pair_sum<T: PartialOrd + Copy>(a: T, b: T) -> T {
    if a > b { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generics_work_across_types() {
        assert_eq!(largest(&[1, 5, 3]), Some(&5));
        assert_eq!(largest(&["a", "z", "m"]), Some(&"z"));
        assert_eq!(largest::<i32>(&[]), None);
    }

    #[test]
    fn trait_default_implementation() {
        let tweet = Tweet {
            user: "torin".to_owned(),
        };
        assert_eq!(tweet.summarize(), "@torin 的推文");
        assert_eq!(tweet.source(), "匿名"); // 用了默认实现
    }

    #[test]
    fn trait_overridden_implementation() {
        let article = Article {
            title: "所有权".to_owned(),
            author: "Steve".to_owned(),
        };
        assert_eq!(article.summarize(), "《所有权》— Steve");
        assert_eq!(article.source(), "Steve");
    }

    #[test]
    fn bound_forms_are_equivalent() {
        let article = Article {
            title: "T".to_owned(),
            author: "A".to_owned(),
        };
        assert_eq!(notify(&article), notify_generic(&article));
        assert_eq!(pair_sum(3, 9), 9);
    }
}
