//! Pi 接入；对外只暴露底座类型，RPC 协议封装在内部。
mod rpc;

pub use rpc::PiAgent;
