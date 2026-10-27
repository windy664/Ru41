//! 边界 domain 类型统一定义在 ru41-plugin-api（宿主与插件同一份），
//! 本文件只做路径保持；error/schedule 是宿主内部域，不跨插件边界。
pub mod error;
pub mod schedule;
pub mod todo;

pub use ru41_plugin_api::domain::*;
