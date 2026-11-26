//! `[tingwu]` 配置段（插件目录配置：`plugins.d/tingwu/config.toml`）。
//!
//! ⚠️ **cookie 不进配置**：认证串放 ru41 的**凭据托管库**，这里只写「凭据叫什么」。
//! 真值不进仓库、不进模型上下文 —— 与其它附属同一套规矩。

use std::path::PathBuf;
use std::time::Duration;

/// 通义听悟附属的配置段。
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TingwuSection {
    /// 凭据托管库里存听悟 cookie 的那条凭据（标签或别名）。
    /// 缺省 `tingwu-cookie`。
    pub credential: Option<String>,
    /// 轮询转写结果的超时（秒），默认 900（15 分钟）。
    pub poll_timeout_secs: Option<u64>,
    /// 轮询间隔（秒），默认 5。
    pub poll_interval_secs: Option<u64>,
    /// 逐字稿落盘目录；缺省落在 state.db 同级的 `tingwu/`（容器内即 `/data/tingwu`）。
    pub output_dir: Option<PathBuf>,
    /// 单个文件大小上限（MB），默认 500（听悟网页版的单文件限制）。
    pub max_file_mb: Option<u64>,
    /// 同一人两句话间隔小于这个毫秒数就并进同一段（默认 5000）。
    /// 调小 → 分段更碎；调大 → 一段更长。
    pub merge_gap_ms: Option<i64>,
    /// 听悟失败（配额用完 / 网络挂了 / cookie 过期）时是否自动回落本地 ASR。
    /// 缺省 true —— 本地那条路没有说话人分离，但至少有字。
    pub fallback_local: Option<bool>,
    /// 本地 ASR 的基址（容器间服务名）。缺省 `http://paraformer-asr:50000`。
    pub local_asr_base_url: Option<String>,
    /// 本地 ASR 的接口形态：`whispercpp`（默认）或 `openai`。
    pub local_asr_api: Option<String>,
}

impl TingwuSection {
    /// 凭据的名字（标签/别名/唯一标识）。
    pub fn credential(&self) -> &str {
        self.credential
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(crate::CREDENTIAL_LABEL)
    }

    /// 轮询超时（至少 30 秒 —— 短于这个必然来不及转写完）。
    pub fn poll_timeout(&self) -> Duration {
        Duration::from_secs(self.poll_timeout_secs.unwrap_or(900).max(30))
    }

    /// 轮询间隔（至少 2 秒，免得把听悟打爆）。
    pub fn poll_interval(&self) -> Duration {
        Duration::from_secs(self.poll_interval_secs.unwrap_or(5).max(2))
    }

    /// 单文件大小上限（字节）。
    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_mb.unwrap_or(500).max(1) * 1024 * 1024
    }

    /// 同一人合并成一段的间隔阈值（毫秒）。
    pub fn merge_gap_ms(&self) -> i64 {
        self.merge_gap_ms.unwrap_or(5_000).max(0)
    }

    /// 听悟失败要不要自动回落本地 ASR（缺省要）。
    pub fn fallback_local(&self) -> bool {
        self.fallback_local.unwrap_or(true)
    }

    /// 本地 ASR 基址（容器间服务名）。
    pub fn local_asr_base_url(&self) -> String {
        self.local_asr_base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("http://paraformer-asr:50000")
            .trim_end_matches('/')
            .to_owned()
    }

    /// 本地 ASR 接口形态（`whispercpp` / `openai`）。
    pub fn local_asr_api(&self) -> String {
        self.local_asr_api
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("whispercpp")
            .to_ascii_lowercase()
    }
}

impl TingwuSection {
    /// 转成宿主侧客户端参数（客户端已上移到 `ru41::adapters::tingwu`）。
    pub fn client_config(&self) -> ru41::adapters::tingwu::TingwuConfig {
        ru41::adapters::tingwu::TingwuConfig {
            poll_timeout: self.poll_timeout(),
            poll_interval: self.poll_interval(),
            max_file_bytes: self.max_file_bytes(),
            merge_gap_ms: self.merge_gap_ms(),
            local_asr_base_url: self.local_asr_base_url(),
            local_asr_api: self.local_asr_api(),
        }
    }
}
