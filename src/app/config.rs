use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 顶层配置；全部字段可选，缺省走内置默认值。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub store: StoreConfig,
    pub worker: WorkerSection,
    pub agent: AgentSection,
    pub channel: ChannelSection,
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

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentSection {
    pub codex: CodexSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodexSection {
    pub bin: Option<PathBuf>,
    pub model: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelSection {
    pub qq: QqSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QqSection {
    pub app_id: Option<String>,
    pub app_secret: Option<String>,
}

impl Config {
    /// 加载配置：显式路径 > ./ru41.toml > ~/.config/ru41/config.toml > 全默认。
    /// 返回配置与实际使用的文件路径（未使用配置文件时为 None）。
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let cwd = std::env::current_dir().context("cannot resolve current directory")?;
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::load_from(explicit, &cwd, home.as_deref())
    }

    /// 可注入目录的加载逻辑，便于测试优先级。
    pub fn load_from(
        explicit: Option<&Path>,
        cwd: &Path,
        home: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>)> {
        let candidates: Vec<PathBuf> = match explicit {
            Some(path) => vec![path.to_owned()],
            None => {
                let mut candidates = vec![cwd.join("ru41.toml")];
                if let Some(home) = home {
                    candidates.push(home.join(".config/ru41/config.toml"));
                }
                candidates
            }
        };
        for path in candidates {
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
