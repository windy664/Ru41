use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 顶层配置；全部字段可选，缺省走内置默认值。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub store: StoreConfig,
    pub worker: WorkerSection,
    pub todo: TodoSection,
    pub obsidian: ObsidianSection,
    pub knowledge: KnowledgeSection,
    pub agent: AgentSection,
    pub channel: ChannelSection,
    pub api: ApiSection,
    pub sources: SourcesSection,
    pub notify: NotifySection,
}

/// HTTP API 配置；令牌也允许环境变量 RU41_API_TOKEN 注入（优先级更高）。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApiSection {
    /// 监听地址，如 "127.0.0.1:4141"。
    pub listen: Option<String>,
    /// Bearer 令牌（至少 24 字节）；密钥类首选环境变量。
    pub token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// sqlite 或 postgres。
    pub kind: Option<String>,
    /// sqlite 数据库文件路径。
    pub path: Option<PathBuf>,
    /// postgres 连接串。
    pub url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerSection {
    pub timeout_secs: Option<u64>,
    pub runs_dir: Option<PathBuf>,
    pub sanitize: Option<bool>,
}

/// 知识库向量检索（RAG）配置；索引库与模型缓存都是可重建的衍生数据。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KnowledgeSection {
    /// 向量库文件路径；默认与 state.db 同目录的 knowledge.db。
    pub db_path: Option<PathBuf>,
    /// embedding 模型缓存目录；默认与 state.db 同目录的 fastembed/。
    pub cache_dir: Option<PathBuf>,
}

impl KnowledgeSection {
    /// 解析最终路径：显式配置优先，否则落在 state.db 旁边（容器内即 /data）。
    pub fn resolved(&self, state_db: &Path) -> (PathBuf, PathBuf) {
        let dir = state_db
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let db = self
            .db_path
            .clone()
            .unwrap_or_else(|| dir.join("knowledge.db"));
        let cache = self
            .cache_dir
            .clone()
            .unwrap_or_else(|| dir.join("fastembed"));
        (db, cache)
    }
}

/// 私人待办视图的本地日期与窗口配置。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TodoSection {
    /// 本地时区固定偏移（分钟），默认 +08:00。
    pub utc_offset_minutes: Option<i64>,
    /// 今日之后归入“近期”的天数，默认 7。
    pub recent_days: Option<i64>,
    /// 超过该天数的已排期事项归入“长期”，默认 14。
    pub long_term_days: Option<i64>,
}

impl TodoSection {
    pub fn options(&self) -> Result<crate::domain::todo::TodoOptions> {
        let minutes = self.utc_offset_minutes.unwrap_or(480);
        anyhow::ensure!(
            (-12 * 60..=14 * 60).contains(&minutes),
            "todo.utc_offset_minutes must be between -720 and 840"
        );
        crate::domain::todo::TodoOptions {
            offset_seconds: i32::try_from(minutes * 60)?,
            recent_days: self.recent_days.unwrap_or(7),
            long_term_days: self.long_term_days.unwrap_or(14),
        }
        .validated()
    }
}

/// Obsidian Vault 闭环配置（核心能力，非附属）：Git 轮询发现 Inbox 新提交，
/// 入队 Agent 整理任务，成功后提交推送；其他附属经 `ports::vault` 读写 Vault。
/// 仓库只存服务器本地路径与远端名，凭据走 git credential helper，不写进配置。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObsidianSection {
    /// 服务器上的 Vault 克隆路径（容器内路径，如 /vault）。
    pub vault_path: Option<String>,
    /// 远端与分支；默认 origin/master。
    pub remote: Option<String>,
    pub branch: Option<String>,
    /// 自动整理的目录前缀；默认 00-Inbox-收件箱。
    pub inbox_dir: Option<String>,
    /// obsidian run 的轮询间隔（秒），默认 300。
    pub interval_secs: Option<u64>,
    /// 整理 digest 任务的执行底座，默认 codex。
    pub backend: Option<String>,
    /// 整理完成后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
}

impl ObsidianSection {
    /// run 的轮询间隔（秒）默认值；装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 300;
    pub const DEFAULT_REMOTE: &str = "origin";
    pub const DEFAULT_BRANCH: &str = "master";
    pub const DEFAULT_INBOX_DIR: &str = "00-Inbox-收件箱";

    /// 配置是否已齐（schedule list 的 enabled 判定与装配共用同一套检查）。
    pub fn is_ready(&self) -> bool {
        self.vault_path
            .as_deref()
            .is_some_and(|p| !p.trim().is_empty())
    }

    /// 由 [obsidian] 解析出运行时配置；默认值只定义在这里。
    /// 通知路由由装配层按 [notify] 渠道注入，核心内不感知渠道知识。
    pub fn resolved(
        &self,
        notify: Option<crate::domain::NotifyRoute>,
    ) -> Result<crate::app::obsidian::ObsidianConfig> {
        let vault_path = self
            .vault_path
            .clone()
            .filter(|p| !p.trim().is_empty())
            .context("obsidian.vault_path required")?;
        Ok(crate::app::obsidian::ObsidianConfig {
            vault_path: std::path::PathBuf::from(vault_path),
            remote: self
                .remote
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_REMOTE.into()),
            branch: self
                .branch
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_BRANCH.into()),
            inbox_dir: self
                .inbox_dir
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_INBOX_DIR.into()),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            backend: self.backend.clone().unwrap_or_else(|| "codex".into()),
            notify,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSection {
    pub pi: PiSection,
    pub codex: CodexSection,
    pub skills: AgentSkillsSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSkillsSection {
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexSection {
    pub bin: Option<PathBuf>,
    pub model: Option<String>,
    pub output_schema: Option<bool>,
    pub config_overrides: Vec<String>,
    pub provider: Option<CodexProviderSection>,
}

pub use crate::adapters::agent::codex::provider::CodexProviderSection;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelSection {
    pub qq: QqSection,
    pub feishu: FeishuSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QqSection {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
    /// 身份绑定口令；未配置时 QQ 的「绑定」命令不可用。
    pub bind_secret: Option<String>,
    /// 主动通知的默认目标（如 user:{openid}）；QQ 主动推送受平台配额限制。
    pub default_notify_target: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeishuSection {
    /// 自定义群机器人 webhook URL（含 token，建议只用配置文件或环境变量注入）。
    pub webhook_url: Option<String>,
}

/// 默认通知出口：业务模块只说「要通知」，用哪个渠道由这里统一决定。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NotifySection {
    /// 渠道标识（feishu / qq）；缺省表示无默认出口，简报类任务只入队不推送。
    pub channel: Option<String>,
}

/// 附属来源配置由各自业务模块定义，这里只做组合。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcesSection {
    pub arxiv: crate::modules::papers::ArxivSection,
    pub rss: crate::modules::rss::RssSection,
    pub projects: crate::modules::projects::ProjectsSection,
    pub youtube: crate::modules::youtube::YoutubeSection,
    pub ecard: crate::modules::ecard::EcardSection,
    pub chaoxing: crate::modules::chaoxing::ChaoxingSection,
    pub jwxt: crate::modules::jwxt::JwxtSection,
    pub zhihuishu: crate::modules::zhihuishu::ZhihuishuSection,
    pub teachermate: crate::modules::teachermate::TeachermateSection,
    pub blog: crate::modules::blog::BlogSection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    pub path: PathBuf,
    pub exists: bool,
}

fn candidates(explicit: Option<&Path>, cwd: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    match explicit {
        Some(path) => vec![path.to_owned()],
        None => {
            let mut candidates = vec![cwd.join("ru41.toml")];
            if let Some(home) = home {
                candidates.push(home.join(".config/ru41/config.toml"));
            }
            // 容器部署的标准位置（compose 把配置挂在这里）。
            candidates.push(PathBuf::from("/etc/ru41/ru41.toml"));
            candidates
        }
    }
}

impl Config {
    /// 加载配置：显式路径 > ./ru41.toml > ~/.config/ru41/config.toml > 全默认。
    /// 返回配置与实际使用的文件路径（未使用配置文件时为 None）。
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let cwd = std::env::current_dir().context("cannot resolve current directory")?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::load_from(explicit, &cwd, home.as_deref())
    }

    pub fn locate(explicit: Option<&Path>) -> Result<ConfigLocation> {
        let cwd = std::env::current_dir().context("cannot resolve current directory")?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Ok(Self::locate_from(explicit, &cwd, home.as_deref()))
    }

    pub fn locate_from(explicit: Option<&Path>, cwd: &Path, home: Option<&Path>) -> ConfigLocation {
        for path in candidates(explicit, cwd, home) {
            if path.is_file() {
                return ConfigLocation { path, exists: true };
            }
        }
        ConfigLocation {
            path: explicit
                .map(Path::to_owned)
                .unwrap_or_else(|| cwd.join("ru41.toml")),
            exists: false,
        }
    }

    /// 可注入目录的加载逻辑，便于测试优先级。
    pub fn load_from(
        explicit: Option<&Path>,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>)> {
        for path in candidates(explicit, cwd, home) {
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read config file {}", path.display()))?;
                let config: Config = toml::from_str(&text)
                    .with_context(|| format!("invalid config file {}", path.display()))?;
                return Ok((config, Some(path)));
            }
        }
        if let Some(path) = explicit {
            anyhow::bail!("config file not found: {}", path.display());
        }
        Ok((Self::default(), None))
    }
}

/// Pi RPC 分析配置；默认不注册，认证与自定义模型由 Pi 原生配置管理。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PiSection {
    pub enabled: bool,
    pub bin: Option<PathBuf>,
    pub model: Option<String>,
    pub provider: Option<String>,
}
