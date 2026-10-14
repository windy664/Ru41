use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Ru41: Rust + Codex 个人自动化底座")]
pub struct Cli {
    /// 配置文件路径；缺省依次尝试 ./ru41.toml、~/.config/ru41/config.toml。
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,
    /// 本地运行数据路径；不要把数据库放在 Vault 里。
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    /// 存储后端：sqlite 或 postgres。
    #[arg(long, global = true, value_parser = ["sqlite", "postgres"])]
    pub store: Option<String>,
    /// PostgreSQL 连接串（postgres 存储时使用，也可用环境变量 RU41_PG_URL）。
    #[arg(long, global = true, env = "RU41_PG_URL")]
    pub pg_url: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 启动 HTTP API；任务由独立 worker 执行。
    Serve {
        #[arg(long, default_value = "127.0.0.1:4141")]
        listen: std::net::SocketAddr,
    },
    /// 检查本地 Codex CLI 是否可用；不调用模型。
    Doctor,
    /// 列出已注册的运行时底座及其支持的契约。
    Backends,
    /// 执行队列中的分析任务；底座由任务的 backend_id 路由决定。
    Worker {
        /// 最多运行一个就绪任务后退出。
        #[arg(long)]
        once: bool,
        #[arg(long)]
        runs_dir: Option<PathBuf>,
        #[arg(long)]
        codex_bin: Option<PathBuf>,
        #[arg(long)]
        model: Option<String>,
        /// 关闭 Agent 调用前的信息脱敏（调试用）。
        #[arg(long)]
        no_sanitize: bool,
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86400))]
        timeout_secs: Option<u64>,
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
    /// QQ 开放平台 AppID（也可用环境变量 RU41_QQ_APP_ID 或配置文件）。
    #[arg(long, env = "RU41_QQ_APP_ID")]
    pub app_id: Option<String>,
    /// QQ 开放平台 ClientSecret（也可用环境变量 RU41_QQ_APP_SECRET 或配置文件）。
    #[arg(long, env = "RU41_QQ_APP_SECRET")]
    pub app_secret: Option<String>,
}

#[derive(Subcommand)]
pub enum QqCommand {
    /// 长驻运行消息网关（WebSocket 收消息 + 结果通知循环）。
    Run {
        #[command(flatten)]
        credentials: QqCredentials,
        /// 消息任务入队时绑定的底座。
        #[arg(long)]
        backend: Option<String>,
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
