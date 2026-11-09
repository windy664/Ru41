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
    pub plugins: PluginsSection,
    /// 供应商定价（用量报告折算花费用）。
    pub pricing: PricingSection,
}

/// 动态插件（cdylib）配置。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginsSection {
    /// 插件目录；缺省 /etc/ru41/plugins，目录不存在视为无插件。
    pub dir: Option<PathBuf>,
    /// 显式关闭插件加载（默认开）。
    pub enabled: Option<bool>,
}

impl PluginsSection {
    pub fn dir_or_default(&self) -> PathBuf {
        self.dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("/etc/ru41/plugins"))
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
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
///
/// `resolved()`（产出宿主侧 ObsidianConfig）留在宿主 app::obsidian 的扩展
/// trait，本 crate 只承载数据与默认值。
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
    /// 整理 digest 任务的执行底座，默认 pi。
    pub backend: Option<String>,
    /// 整理完成后是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// rebase 冲突时用 LLM 语义合并；默认 true（无 key 时降级为保留现场+通知）。
    pub merge_ai: Option<bool>,
    /// 合并用端点；不写就用底座供应商（[agent.pi]）的端点。
    pub merge_base_url: Option<String>,
    /// 合并用模型；不写就用底座供应商的模型。
    pub merge_model: Option<String>,
    /// 合并 key 的环境变量名；不写就用底座供应商的密钥变量。
    pub merge_api_key_env: Option<String>,
    /// 合并用协议：anthropic-messages（默认）/ openai-completions；
    /// 不写就跟底座供应商一致。
    pub merge_api: Option<String>,
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
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSection {
    pub pi: PiSection,
    pub codex: CodexSection,
    pub skills: AgentSkillsSection,
    /// 全局默认底座：QQ 入口、CLI 入队、插件未显式指定时都用它。
    /// 统一底座就改这一处；不写则用 [`AgentSection::DEFAULT_BACKEND`]。
    pub default_backend: Option<String>,
}

impl AgentSection {
    /// 默认底座 id。
    pub const DEFAULT_BACKEND: &'static str = "pi";

    /// 生效的默认底座：配置优先，空值回落默认。
    pub fn default_backend(&self) -> &str {
        self.default_backend
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(Self::DEFAULT_BACKEND)
    }
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
    /// 底座沙箱：「bypass」（默认，维持全权限）或「workspace-write」
    /// （限制为工作区写入；agent 命令需要联网时还需在 codex 配置
    /// `[sandbox_workspace_write] network_access = true`）。
    pub sandbox: Option<String>,
}

/// 自定义模型提供方：写入 codex 的 model_providers.<id>，
/// 密钥经 api_key_env 指定的环境变量注入，不落入配置文件。
/// 类型属于配置边界（CodexSection 字段），翻译逻辑留在宿主 adapters。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexProviderSection {
    /// provider 标识，也是 model_provider 选择键。
    pub id: Option<String>,
    pub name: Option<String>,
    /// 兼容端点地址，如 https://api.example.com/v1。
    pub base_url: Option<String>,
    /// 存放 API 密钥的环境变量名（如 OPENROUTER_API_KEY）。
    pub api_key_env: Option<String>,
    /// chat（OpenAI Chat Completions 兼容）或 responses（Responses API）。
    pub wire_api: Option<String>,
}

/// 供应商定价（人民币 / 百万 token）。
///
/// 用量报告要回答「实际花了多少钱」，光给 token 数看不懂。价格随供应商调整，
/// 所以默认走内置值（MiniMax-M3，2026-11 的公开价折合人民币），配置里可覆盖。
/// 内置值参考：输入 $0.30/M、输出 $1.20/M、缓存命中 $0.06/M（按 1 美元 ≈ 7.2 元折算）。
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PricingSection {
    /// 默认输入价（元 / 百万 token）。
    pub input_cny_per_million: Option<f64>,
    /// 默认输出价。
    pub output_cny_per_million: Option<f64>,
    /// 默认缓存命中价。
    pub cache_read_cny_per_million: Option<f64>,
    /// 分模型覆盖（键为模型名，可写带供应方前缀的完整名）。
    pub models: std::collections::BTreeMap<String, ModelPricing>,
}

/// 单个模型的定价（元 / 百万 token）。
#[derive(Debug, Clone, Default, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelPricing {
    pub input_cny_per_million: Option<f64>,
    pub output_cny_per_million: Option<f64>,
    pub cache_read_cny_per_million: Option<f64>,
}

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
    /// 静默来源：这些后台服务的 agent 过程不向用户汇报（终态通知 + 活动流都不推）。
    /// 匹配 request_key 的前缀段（段数不限）：配 "obsidian" 命中
    /// "obsidian:organize:xxx"；配 "rss:opportunities" 只命中机会雷达那类。
    pub silent_sources: Option<Vec<String>>,
}

/// 附属来源配置按模块名分布式解析：新增模块不再往这里加字段，
/// 各自的 Section 类型留在模块目录里，读取走 `get::<T>(name)`。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct SourcesSection {
    #[serde(flatten)]
    modules: std::collections::HashMap<String, toml::Value>,
}

impl SourcesSection {
    /// 按模块名解析配置段（Result 上下文用）；段缺失返回 Default，
    /// 存在但解析失败报错——字段拼错由模块 Section 的 deny_unknown_fields 兜住。
    pub fn get<T: Default + serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        match self.modules.get(name) {
            None => Ok(T::default()),
            Some(value) => value
                .clone()
                .try_into()
                .with_context(|| format!("[sources.{name}] 配置解析失败")),
        }
    }

    /// 合并插件目录配置：表递归合并，标量与数组由后加载的层覆盖。
    /// 主配置 `[sources]` 先加载，plugins.d/<插件>/config.toml 其后，
    /// config.local.toml 最后，因此设备本地值永远优先。
    pub fn merge_module_table(&mut self, overlay: toml::Table) {
        for (name, value) in overlay {
            match self.modules.get_mut(&name) {
                Some(existing) => merge_toml_value(existing, value),
                None => {
                    self.modules.insert(name, value);
                }
            }
        }
    }

    /// 非 Result 上下文（如 schedule 清单闭包）用：解析失败记日志并退回 Default
    /// （等价于"未配置"，模块静默缺席）。
    pub fn get_or_default<T: Default + serde::de::DeserializeOwned>(&self, name: &str) -> T {
        self.get(name).unwrap_or_else(|error| {
            eprintln!("[config] {error:#}；按未配置处理");
            T::default()
        })
    }

    /// 配置段是否存在（status 等只做"配没配"判断，不解析具体字段）。
    pub fn has(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// 已出现的配置段名（宿主做未知键告警用；注册表知识在宿主侧）。
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.modules.keys().map(String::as_str)
    }
}

fn merge_toml_value(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_toml_value(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
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

    /// 插件目录配置的默认根：主配置旁边的 plugins.d/。
    /// 运行时布局是 plugins.d/<插件名>/config.toml（可选 config.local.toml 覆盖）。
    pub fn default_plugin_config_root(config_path: Option<&Path>, cwd: &Path) -> PathBuf {
        config_path
            .and_then(Path::parent)
            .unwrap_or(cwd)
            .join("plugins.d")
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
        let plugin_root = std::env::var_os("RU41_PLUGIN_CONFIG_ROOT").map(PathBuf::from);
        Self::load_from_with_plugin_root(explicit, cwd, home, plugin_root.as_deref())
    }

    /// 测试与部署可显式指定插件配置根；None 时取主配置旁边的 plugins.d/。
    pub fn load_from_with_plugin_root(
        explicit: Option<&Path>,
        cwd: &Path,
        home: Option<&Path>,
        plugin_root: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>)> {
        let mut config_path = None;
        let mut config = Self::default();
        for path in candidates(explicit, cwd, home) {
            if path.is_file() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read config file {}", path.display()))?;
                config = toml::from_str(&text)
                    .with_context(|| format!("invalid config file {}", path.display()))?;
                config_path = Some(path);
                break;
            }
        }
        if config_path.is_none()
            && let Some(path) = explicit
        {
            anyhow::bail!("config file not found: {}", path.display());
        }
        let root = plugin_root
            .map(Path::to_path_buf)
            .unwrap_or_else(|| Self::default_plugin_config_root(config_path.as_deref(), cwd));
        config.load_plugin_config_root(&root)?;
        Ok((config, config_path))
    }

    /// 合并 plugins.d/<插件名>/{config.toml,config.local.toml}。
    /// 每个文件顶层直接写配置段名，例如 papers/config.toml 里的 `[arxiv]`。
    pub fn load_plugin_config_root(&mut self, root: &Path) -> Result<()> {
        if !root.is_dir() {
            return Ok(());
        }
        let mut plugin_dirs = Vec::new();
        for entry in std::fs::read_dir(root)
            .with_context(|| format!("cannot read plugin config root {}", root.display()))?
        {
            let path = entry?.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_dir() && !name.starts_with('.') {
                plugin_dirs.push(path);
            }
        }
        plugin_dirs.sort();
        for dir in plugin_dirs {
            for file_name in ["config.toml", "config.local.toml"] {
                let path = dir.join(file_name);
                if !path.is_file() {
                    continue;
                }
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read plugin config {}", path.display()))?;
                let value: toml::Value = toml::from_str(&text)
                    .with_context(|| format!("invalid plugin config {}", path.display()))?;
                let table = value.as_table().cloned().with_context(|| {
                    format!("plugin config {} must be a TOML table", path.display())
                })?;
                self.sources.merge_module_table(table);
            }
        }
        Ok(())
    }
}

/// Pi 的默认输入模态：文本 + 图片。
fn default_input_modalities() -> Vec<String> {
    vec!["text".to_owned(), "image".to_owned()]
}

/// Pi RPC 分析配置；默认不注册，认证与自定义模型由 Pi 原生配置管理。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PiSection {
    pub enabled: bool,
    pub bin: Option<PathBuf>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// 自定义端点：写入运行目录下的临时 agent 目录（models.json），
    /// Pi 通过该文件识别兼容端点。密钥不写进文件。
    pub base_url: Option<String>,
    /// 端点协议：anthropic-messages（默认）/ openai-completions / openai-responses。
    pub api: Option<String>,
    /// 密钥来源的环境变量名：值只注入 Pi 子进程环境，
    /// models.json 里只写 `$NAME` 引用，密钥不落配置与日志。
    pub api_key_env: Option<String>,
    /// 会话持久化：开启后可按 session_id 恢复上下文。
    pub session: Option<bool>,
    /// 会话存储目录（对应 Pi 的 --session-dir）。
    pub session_dir: Option<PathBuf>,
    /// 备用模型链：主模型失败时按顺序切换；每项写 `供应方/模型` 或裸 `模型`。
    pub fallback_models: Vec<String>,
    /// 模型输入模态（写进 models.json 的 `input`）：text / image。
    /// Pi 默认只认文本，声明 image 后它的 read 工具才会把图片作为附件发给模型。
    /// 缺省 ["text","image"]——模型本身不支持图片时由 Pi 侧忽略附件。
    #[serde(default = "default_input_modalities")]
    pub input: Vec<String>,
    /// 工具白名单（Pi 的 --tools，逗号分隔，如 "read,write,edit,bash"）。
    /// 留空则禁用全部工具：只做文本分析。启用后 Pi 的工具没有操作系统级沙箱，
    /// 权限与 Codex 的 bypass 模式相当。
    pub tools: Option<String>,
    /// Pi 扩展（--extension）：显式路径列表。
    ///
    /// 只影响这里写下的扩展——Pi 的扩展发现始终是关的（--no-extensions 保留），
    /// 不会因为装了扩展包就自动多出工具。浏览器扩展示例见 docs/browser-automation.md。
    pub extensions: Vec<String>,
    /// 额外注入 Pi 子进程的环境变量（键值对）。
    ///
    /// 扩展要自己读环境变量时用这个：例如浏览器扩展读 `BU_CDP_WS`
    /// 找 CDP 端点。值只进子进程环境，不写进日志。
    pub extra_env: std::collections::BTreeMap<String, String>,
}
