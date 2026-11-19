//! 块 3 · 所有权（下）—— 切片 + 结构体 + 方法 —— Rust Book Ch4.3 (Slices) + Ch5 (Structs)
//!
//! 跑法：`rustc --edition 2024 --test block03_structs.rs -o /tmp/b03 && /tmp/b03`
//!
//! 本块产出：能用 struct + impl 组织数据，并用切片安全地取子串。

/// Ch4.3 切片借用原数据，不复制；带生命周期标注（这里靠编译器推断）。
fn first_word(text: &str) -> &str {
    text.split_whitespace().next().unwrap_or("")
}

/// Ch4.3 字节切片与 UTF-8 边界：不能用会切坏字符的索引。
fn byte_len(text: &str) -> usize {
    text.len()
}

/// Ch5.1 定义并实例化结构体。
#[derive(Debug, Clone, PartialEq)]
struct Rectangle {
    width: u32,
    height: u32,
}

/// Ch5.3 方法：`impl` 块 + `self`。
impl Rectangle {
    /// 构造器惯例：返回 Self 而不是裸结构体。
    fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// 不可变方法。
    fn area(&self) -> u32 {
        self.width * self.height
    }

    /// 可变方法：需要 `&mut self`。
    fn scale(&mut self, factor: u32) {
        self.width *= factor;
        self.height *= factor;
    }

    /// 关联函数（没有 self）：常用于构造器。
    fn square(size: u32) -> Self {
        Self::new(size, size)
    }
}

/// Ch5.2 字段简写语法。
fn make_rect(w: u32, h: u32) -> Rectangle {
    Rectangle { width: w, height: h }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_borrow_without_copying() {
        let text = String::from("hello world");
        assert_eq!(first_word(&text), "hello");
        assert_eq!(text, "hello world");
    }

    #[test]
    fn utf8_len_is_bytes_not_chars() {
        assert_eq!(byte_len("Rust"), 4);
        assert_eq!(byte_len("语言"), 6); // 3 字节 × 2
        assert_eq!("语言".chars().count(), 2);
    }

    #[test]
    fn struct_methods() {
        let mut r = Rectangle::new(3, 4);
        assert_eq!(r.area(), 12);
        r.scale(2);
        assert_eq!(r.area(), 48);
        assert_eq!(Rectangle::square(5).area(), 25);
        assert_eq!(make_rect(2, 3), Rectangle::new(2, 3));
    }
}
