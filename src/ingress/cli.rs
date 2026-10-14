use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Ru41: Rust + Codex 个人自动化底座")]
pub struct Cli {
    /// 本地运行数据路径；不要把数据库放在 Vault 里。
    #[arg(long, global = true, default_value = ".ru41/state.db")]
    pub db: PathBuf,
    /// 存储后端：sqlite（默认）或 postgres。
    #[arg(long, global = true, default_value = "sqlite", value_parser = ["sqlite", "postgres"])]
    pub store: String,
    /// PostgreSQL 连接串（--store postgres 时使用，也可用环境变量 RU41_PG_URL）。
    #[arg(long, global = true, env = "RU41_PG_URL")]
    pub pg_url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 检查本地 Codex CLI 是否可用；不调用模型。
    Doctor,
    /// 列出已注册的运行时底座及其支持的契约。
    Backends,
    /// 通过 Codex 执行队列中的分析任务（使用你的 Codex 认证）。
    Worker {
        #[arg(long, default_value = "codex")]
        backend: String,
        /// 最多运行一个就绪任务后退出。
        #[arg(long)]
        once: bool,
        #[arg(long, default_value = ".ru41/runs")]
        runs_dir: PathBuf,
        #[arg(long, default_value = "codex")]
        codex_bin: PathBuf,
        #[arg(long)]
        model: Option<String>,
        /// 关闭 Agent 调用前的信息脱敏（调试用）。
        #[arg(long)]
        no_sanitize: bool,
        #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout_secs: u64,
    },
    /// 存储和查看分析请求及其执行历史。
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    /// QQ 官方机器人消息网关：收消息入队任务，完成后回复原会话。
    Qq {
        #[command(subcommand)]
        command: QqCommand,
    },
}

#[derive(clap::Args)]
pub struct QqCredentials {
    /// QQ 开放平台 AppID（也可用环境变量 RU41_QQ_APP_ID）。
    #[arg(long, env = "RU41_QQ_APP_ID")]
    pub app_id: String,
    /// QQ 开放平台 ClientSecret（也可用环境变量 RU41_QQ_APP_SECRET）。
    #[arg(long, env = "RU41_QQ_APP_SECRET")]
    pub app_secret: String,
}

#[derive(Subcommand)]
pub enum QqCommand {
    /// 长驻运行消息网关（WebSocket 收消息 + 结果通知循环）。
    Run {
        #[command(flatten)]
        credentials: QqCredentials,
        /// 消息任务路由到的底座。
        #[arg(long, default_value = "codex")]
        backend: String,
    },
    /// 手动向群发送一条主动文本消息（用于测试连通性）。
    Send {
        #[command(flatten)]
        credentials: QqCredentials,
        #[arg(long)]
        group_openid: String,
        #[arg(long)]
        text: String,
    },
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// 入队一个请求。相同事件用相同的 key 与 prompt 重试。
    Add {
        #[arg(long, default_value = "codex")]
        backend: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        prompt: String,
    },
    List {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Show {
        id: i64,
    },
    /// 显示全部执行尝试，包括错误与本地产物路径。
    Attempts {
        id: i64,
    },
    /// 手动重试失败任务，追加新的尝试预算。
    Retry {
        id: i64,
    },
    /// 取消排队中或运行中的请求。重复取消是安全的。
    Cancel {
        id: i64,
    },
}
