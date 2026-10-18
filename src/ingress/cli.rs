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
    /// 初始化配置、数据目录和数据库；不接收或写入密钥。
    Init {
        #[arg(long)]
        non_interactive: bool,
        #[arg(long, value_parser = ["codex", "mimo"])]
        provider: Option<String>,
    },
    /// 启动 HTTP API；任务由独立 worker 执行。
    Serve {
        /// 监听地址；缺省取配置 [api].listen 或 127.0.0.1:4141。
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
    },
    /// 检查配置与本地运行环境；--deep 才会调用模型。
    Doctor {
        #[arg(long)]
        deep: bool,
    },
    /// 查看或校验配置；敏感值默认脱敏。
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// 查看任务与启用组件概况。
    Status,
    /// 输出适合排障分享的脱敏配置与状态报告。
    Dump,
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
    /// 长驻运行终态通知派发；按配置注册渠道（当前为飞书 webhook）。
    /// QQ 网关在 qq run 内自带派发，此命令用于不跑 QQ 的部署。
    Notify,
    /// arXiv 论文追踪：轮询新论文、去重粗筛后合成精选 digest 任务。
    Papers {
        #[command(subcommand)]
        command: PapersCommand,
    },
    /// RSS 信息流追踪：轮询订阅源、去重粗筛后合成精选 digest 任务。
    Rss {
        #[command(subcommand)]
        command: RssCommand,
    },
    /// 校园卡水电监控：查询余额、记录历史、低余额提醒。
    Ecard {
        #[command(subcommand)]
        command: EcardCommand,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// 输出当前配置文件路径及是否存在。
    Path,
    /// 输出脱敏后的配置。
    Show,
    /// 校验配置和供应商装配，不访问网络或数据库。
    Check,
}

#[derive(Subcommand)]
pub enum PapersCommand {
    /// 执行一轮轮询（适合系统 cron 定时调用）。
    Poll,
    /// 常驻运行，按 sources.arxiv.interval_secs 周期轮询。
    Run,
}

#[derive(Subcommand)]
pub enum RssCommand {
    /// 执行一轮轮询（适合系统 cron 定时调用）。
    Poll,
    /// 常驻运行，按 sources.rss.interval_secs 周期轮询。
    Run,
}

#[derive(Subcommand)]
pub enum EcardCommand {
    /// 查询一次余额并按阈值判断是否提醒（适合系统 cron 每日调用）。
    Poll,
    /// 常驻运行，按 sources.ecard.interval_secs 周期查询。
    Run,
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
        /// 身份绑定口令（也可用环境变量 RU41_QQ_BIND_SECRET 或配置文件 channel.qq.bind_secret）。
        #[arg(long, env = "RU41_QQ_BIND_SECRET")]
        bind_secret: Option<String>,
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
    /// 检查 QQ 凭证和 gateway；不连接 WebSocket、不发送消息。
    Doctor {
        #[command(flatten)]
        credentials: QqCredentials,
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
        /// 终态后经默认通知出口推送（[notify] channel 配置决定渠道）。
        #[arg(long)]
        notify: bool,
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
