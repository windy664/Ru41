/// Agent 底座适配器：AgentBackend 的各类运行时实现。
pub mod agent;
/// 数据库适配器：TaskStore 的各类后端实现。
pub mod database;
pub mod messaging;
/// 外部信息来源适配器：arXiv 等采集协议。
pub mod sources;
