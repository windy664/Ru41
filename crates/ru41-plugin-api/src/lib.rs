//! ru41 宿主与动态插件（cdylib）之间的稳定边界。
//!
//! 本 crate 是唯一允许跨宿主↔插件边界的类型来源：ModuleDef 函数指针签名、
//! ports trait 对象、domain 类型、Config 全部定义在这里。宿主 `ru41` 依赖本
//! crate 并在原路径 re-export；插件经 `ru41`（或本 crate）拿到同一份类型。
//!
//! ## 握手纪律（详见宿主 src/plugins.rs 与 docs/plugin-development.md）
//!
//! - 硬门：magic + 本 crate 版本号 + 外部依赖 stamp（build.rs 从 Cargo.lock
//!   提取 tokio/serde 等 ABI 相关依赖的解析版本做摘要）。三者一致即放行，
//!   宿主业务代码变更不再要求重编插件。
//! - 本 crate 任何公开项的破坏性变更必须 bump Cargo.toml version。
//! - git commit 仅作诊断日志，不再是加载门槛。

pub mod bridge;
pub mod config;
pub mod domain;
pub mod entry;
pub mod identity;
pub mod module;
pub mod ports;
pub mod schedule;
pub mod time;
