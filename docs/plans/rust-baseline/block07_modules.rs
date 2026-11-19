//! 块 7 · 模块与包 —— Rust Book Ch7 (Packages, Crates, and Modules)
//!
//! 跑法：`rustc --edition 2024 --test block07_modules.rs -o /tmp/b07 && /tmp/b07`
//!
//! 本块产出：能用 mod / pub / use 组织代码，理解可见性与路径。
//! 本文件是单文件版的模块树（多文件版要靠 cargo，本机拉不到 crate 就先这么练）。

/// Ch7.2 模块默认私有（对父模块而言）。
mod geometry {
    /// pub 让它对外部可见。
    #[derive(Debug, Clone, PartialEq)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    impl Point {
        pub fn new(x: i32, y: i32) -> Self {
            Self { x, y }
        }
    }

    /// 没有 pub：只能在 geometry 模块内部用。
    fn origin() -> Point {
        Point::new(0, 0)
    }

    /// pub(crate)：整个 crate 内可见（Ch7.2 可见性层级）。
    pub(crate) fn origin_public_within_crate() -> Point {
        origin()
    }

    /// 嵌套模块。
    pub mod shapes {
        /// 枚举：把「形状」建模清楚。
        #[derive(Debug, PartialEq)]
        pub enum Shape {
            Circle { radius: f64 },
            Rect { w: f64, h: f64 },
        }

        impl Shape {
            pub fn area(&self) -> f64 {
                match self {
                    Shape::Circle { radius } => std::f64::consts::PI * radius * radius,
                    Shape::Rect { w, h } => w * h,
                }
            }
        }
    }

    /// Ch7.4 use 引入路径，简化书写。
    pub mod util {
        pub fn double(n: i32) -> i32 {
            n * 2
        }
    }
}

/// Ch7.3 路径：绝对路径 `crate::` 与相对路径 `super::`。
fn absolute_path() -> geometry::Point {
    crate::geometry::Point::new(1, 2)
}

/// Ch7.4 use 之后就能直接用名字。
use geometry::shapes::Shape;
use geometry::util::double;

/// Ch7.4 也可以用 `use ... as ...` 重命名。
use geometry::Point as Coord;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_rules() {
        let p = Coord::new(3, 4);
        assert_eq!((p.x, p.y), (3, 4));
        // origin() 是私有的，从外部调不到 —— 这就是可见性。
        let o = geometry::origin_public_within_crate();
        assert_eq!((o.x, o.y), (0, 0));
    }

    #[test]
    fn nested_modules_and_paths() {
        let s = Shape::Rect { w: 2.0, h: 3.0 };
        assert_eq!(s.area(), 6.0);
        assert_eq!(absolute_path(), Coord::new(1, 2));
    }

    #[test]
    fn use_simplifies_paths() {
        assert_eq!(double(21), 42);
    }
}
