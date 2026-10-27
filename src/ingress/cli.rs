use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Ru41: Rust + Codex 个人自动化底座")]
pub struct Cli {
    /// 配置文件路径；缺省依次尝试 ./ru41.toml、~/.config/ru41/config.toml、/etc/ru41/ru41.toml，
    /// 或环境变量 RU41_CONFIG（容器部署由 compose 注入，agent 免带 --config）。
    #[arg(long, global = true, env = "RU41_CONFIG")]
    pub config: Option<PathBuf>,
    /// 本地运行数据路径；不要把数据库放在 Vault 里。容器由 RU41_DB 注入。
    #[arg(long, global = true, env = "RU41_DB")]
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
    /// 私人常驻待办：今日、近期、长期、逾期和收件箱视图。
    Todo {
        #[command(subcommand)]
        command: TodoCommand,
    },
    /// 行程（日程事件）：课表、会议、约会等时间轴占用；与待办互补。
    Event {
        #[command(subcommand)]
        command: EventCommand,
    },
    /// QQ 官方机器人消息网关：收消息入队任务，完成后回复原会话。
    Qq {
        #[command(subcommand)]
        command: QqCommand,
    },
    /// 长驻运行终态通知派发；按配置注册渠道（当前为飞书 webhook）。
    /// QQ 网关在 qq run 内自带派发，此命令用于不跑 QQ 的部署。
    Notify,
    /// YouTube 英语预习：轮询频道新视频，字幕转写生成重点词句预习单并推送。
    Youtube {
        #[command(subcommand)]
        command: YoutubeCommand,
    },
    /// 学习通签到监控：协议端直连，检测待签活动并提醒，支持手动签到。
    Chaoxing {
        #[command(subcommand)]
        command: ChaoxingCommand,
    },
    /// 微助教签到监控：服务号 openid 直连，检测待签活动并提醒，支持手动签到。
    Teachermate {
        #[command(subcommand)]
        command: TeachermateCommand,
    },
    /// 教务课表监控：CAS 单点登录拉课表，变更检测与明日课表提醒。
    Jwxt {
        #[command(subcommand)]
        command: JwxtCommand,
    },
    /// 附属 tracker 通用入口：`ru41 <模块名> poll|run`（模块名见 `schedule list`；
    /// 有专属子命令的模块如 youtube/chaoxing/teachermate/jwxt 在上方独立变体）。
    #[command(external_subcommand)]
    Tracker(Vec<String>),
    /// 定时任务注册表：统一查看和运行各附属的轮询任务。
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommand,
    },
    /// 知识库向量检索：索引 Obsidian vault，语义搜索笔记内容。
    Knowledge {
        #[command(subcommand)]
        command: KnowledgeCommand,
    },
}

#[derive(Subcommand)]
pub enum KnowledgeCommand {
    /// 全量重建向量索引（索引是衍生数据，可随时重建）。
    Index {
        /// vault 路径；缺省取 [obsidian].vault_path。
        #[arg(long)]
        vault: Option<PathBuf>,
    },
    /// 语义检索：返回最相关的若干笔记块（路径 + 标题 + 片段）。
    Search {
        /// 查询文本，自然语言即可。
        query: String,
        /// 返回条数，默认 5。
        #[arg(long, default_value = "5")]
        limit: usize,
    },
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// 列出所有已登记的定时任务、间隔与启停状态。
    List,
    /// 以长驻方式运行指定任务，等价于对应模块的 run 入口。
    Run {
        /// 任务名（schedule list 的 name 列）。
        job: String,
        /// 覆盖轮询间隔（秒），缺省取任务登记值。
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..=86400))]
        interval_secs: Option<u64>,
        /// 通知渠道，仅 todo-remind 使用；缺省取 [notify] 默认出口。
        #[arg(long, default_value = "qq")]
        channel: String,
        /// 通知目标，仅 todo-remind 使用；缺省取 [notify] 默认出口。
        #[arg(long)]
        target: Option<String>,
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

#[derive(Subcommand, Clone, Copy)]
pub enum TrackerCommand {
    /// 执行一轮轮询（适合系统 cron 定时调用）。
    Poll,
    /// 常驻运行，按该来源配置的 interval_secs 周期轮询。
    Run,
}

#[derive(Subcommand)]
pub enum YoutubeCommand {
    /// 执行一轮轮询（适合系统 cron 定时调用）。
    Poll,
    /// 常驻运行，按 sources.youtube.interval_secs 周期轮询。
    Run,
    /// 手动提交一个视频：拉字幕转写入队预习任务，完成后推送预习单。
    Prep {
        /// YouTube 链接（watch / youtu.be / shorts）或 11 位视频 ID
        url: String,
    },
}

#[derive(Subcommand)]
pub enum ChaoxingCommand {
    /// 采集一轮课程与签到活动并按需提醒（适合系统 cron 调用）。
    Poll,
    /// 常驻运行，按 sources.chaoxing.interval_secs 周期轮询。
    Run,
    /// 采集一轮全课程作业清单：提醒/待办/Obsidian 同步（独立低频任务）。
    Work,
    /// 手动签到一个签到活动（全类型：普通/手势/签到码/位置/二维码/拍照）。
    Sign {
        /// 签到活动 ID（poll 提醒里带的 id）。
        activity_id: String,
        /// 位置签到纬度（缺省用配置 default_lat）。
        #[arg(long)]
        lat: Option<f64>,
        /// 位置签到经度（缺省用配置 default_lon）。
        #[arg(long)]
        lon: Option<f64>,
        /// 位置签到地址文本（缺省用配置 default_addr；会显示在教师端）。
        #[arg(long)]
        addr: Option<String>,
        /// 二维码签到 enc 值（现场二维码内容里的时效 token，过期需重扫）。
        #[arg(long)]
        enc: Option<String>,
        /// 拍照签到照片路径（缺省用配置 photo_path，再缺省用内置照片）。
        #[arg(long)]
        photo: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum TeachermateCommand {
    /// 采集一轮待签列表并按需提醒/自动签（适合系统 cron 调用）。
    Poll,
    /// 常驻运行，按 sources.teachermate.interval_secs 周期轮询。
    Run,
    /// 采集一轮作业清单并按状态提醒（低频，独立于签到轮询）。
    Work,
    /// 投喂 openid：从服务号链接或裸 openid 提取、落盘并验证学籍。
    Bind {
        /// 微信里复制的微助教链接（含 openid=）或 32 位 openid 本身。
        link: String,
    },
    /// 手动签到一个进行中的签到活动（普通/GPS；二维码需微信现场扫）。
    Sign {
        /// 签到活动 ID（poll 提醒里带的 id）。
        sign_id: i64,
        /// 位置签到纬度（缺省用配置 default_lat）。
        #[arg(long)]
        lat: Option<f64>,
        /// 位置签到经度（缺省用配置 default_lon）。
        #[arg(long)]
        lon: Option<f64>,
    },
}

#[derive(Subcommand)]
pub enum JwxtCommand {
    /// 拉取一轮课表并按需提醒（变更检测 + 到点推明日课表；cron 友好）。
    Poll,
    /// 常驻运行，按 sources.jwxt.interval_secs 周期轮询。
    Run,
    /// CAS 验证码登录：校方 CAS 每次账密登录都要图形验证码，只能人工一次，
    /// 成功后 TGT 持久化，后续轮询静默换 ST。
    Login {
        /// 验证码答案；缺省时只下载验证码图片并提示路径。
        #[arg(long)]
        captcha: Option<String>,
    },
    /// 读本地课表缓存回答「今天/明天/某天/本周有什么课」；只读缓存，不触网、不需要登录态。
    Classes {
        /// 时间：缺省今天；支持 tomorrow/明天、week/周、YYYY-MM-DD。
        #[arg(default_value = "today")]
        when: String,
    },
}

#[derive(Subcommand)]
pub enum EventCommand {
    Add {
        /// 自然语言时间：明天14:00-16:00、每周二14:00-15:40、今晚20:00-22:00。
        #[arg(long)]
        when: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        location: Option<String>,
    },
    /// 列出今天起 N 天的发生（含周期事件展开）。
    List {
        #[arg(long, default_value_t = 7)]
        days: i64,
    },
    Cancel {
        /// 完整 ID 或短 ID。
        id: String,
    },
    /// 查询与给定时间段重叠的行程（agent 规划前占用检查）。
    Conflicts {
        /// 自然语言时间，格式同 add --when。
        #[arg(long)]
        when: String,
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
    /// 通过 QQ 官方扫码流程创建/绑定机器人并写入本地凭证。
    Onboard,
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

#[derive(Subcommand)]
pub enum TodoCommand {
    /// 不传视图时返回今日、逾期、近期、长期和收件箱摘要。
    List {
        #[arg(value_enum)]
        view: Option<TodoViewArg>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    Add {
        #[arg(long)]
        title: String,
        /// 支持 YYYY-MM-DD、YYYY-MM-DD HH:MM、今天、明天、后天、N天后。
        #[arg(long)]
        due: Option<String>,
        /// 提醒时间与 --due 使用同一格式。
        #[arg(long)]
        remind: Option<String>,
        #[arg(long = "label", value_delimiter = ',')]
        labels: Vec<String>,
        #[arg(long, value_enum, default_value_t = TodoPriorityArg::Normal)]
        priority: TodoPriorityArg,
    },
    Done {
        /// 完整 ID 或至少不歧义的短 ID 前缀。
        id: String,
    },
    Postpone {
        id: String,
        /// 支持 --due 的日期格式。
        until: String,
    },
    Cancel {
        id: String,
    },
    Restore {
        id: String,
    },
    /// 按 remind_at 发送主动提醒；--once 适合 cron，缺省常驻循环。
    Remind {
        #[arg(long)]
        once: bool,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=86400))]
        interval_secs: u64,
        /// 覆盖 [notify] 默认出口；个人提醒通常为 qq + user:{openid}。
        #[arg(long, default_value = "qq")]
        channel: String,
        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TodoViewArg {
    Today,
    Recent,
    LongTerm,
    Overdue,
    Inbox,
    All,
}

impl TodoViewArg {
    pub fn into_domain(self) -> crate::domain::todo::TodoView {
        match self {
            Self::Today => crate::domain::todo::TodoView::Today,
            Self::Recent => crate::domain::todo::TodoView::Recent,
            Self::LongTerm => crate::domain::todo::TodoView::LongTerm,
            Self::Overdue => crate::domain::todo::TodoView::Overdue,
            Self::Inbox => crate::domain::todo::TodoView::Inbox,
            Self::All => crate::domain::todo::TodoView::All,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum TodoPriorityArg {
    Low,
    #[default]
    Normal,
    High,
}

impl TodoPriorityArg {
    pub fn into_domain(self) -> crate::domain::todo::TodoPriority {
        match self {
            Self::Low => crate::domain::todo::TodoPriority::Low,
            Self::Normal => crate::domain::todo::TodoPriority::Normal,
            Self::High => crate::domain::todo::TodoPriority::High,
        }
    }
}
