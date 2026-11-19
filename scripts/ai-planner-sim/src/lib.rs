//! ai-planner-sim 的库部分：配置 + 纯函数规划核心。
//!
//! 刻意做成 lib + bin 两个 crate：
//! - `planner` 是**纯函数**（不碰 IO），可以直接被集成测试覆盖；
//! - 未来要搬进 Ru41 主仓时，`src/domain/allocation.rs` 基本可以直接照搬。

pub mod config;
pub mod planner;
