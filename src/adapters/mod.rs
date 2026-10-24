/// Agent 底座适配器：AgentBackend 的各类运行时实现。
pub mod agent;
/// 数据库适配器：TaskStore 的各类后端实现。
pub mod database;
/// Embedding 适配器：ports::embedding::Embedder 的实现（本地 fastembed）。
pub mod embedding;
/// 跨模块通用的外部信息来源协议（当前为 RSS/Atom feed）。
pub mod sources;
/// Vault 适配器：ports::Vault 的 git 工作副本实现。
pub mod vault;
