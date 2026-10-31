//! 课堂录音附属配置：`[sources.lecture]`。
//! ASR 走 OpenAI 兼容 `/audio/transcriptions`（默认智谱 GLM-ASR），
//! key 优先取 `api_key` 明文，缺省回退 `api_key_env` 环境变量。

use anyhow::{Context, Result};
use serde::Deserialize;

/// `[sources.lecture]` 配置段；解析出的运行时配置只在 `resolved()` 里给默认值。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LectureSection {
    /// 总开关；显式 false 时模块完全静默（handler 不拦截，worker 不注册）。
    pub enabled: Option<bool>,
    /// ASR API key 明文（推荐直接配置）。
    pub api_key: Option<String>,
    /// ASR API key 的环境变量名；没有 api_key 时使用。默认 ZHIPU_API_KEY。
    pub api_key_env: Option<String>,
    /// ASR base URL（OpenAI 兼容，不含 /audio/transcriptions）。
    /// 默认 https://open.bigmodel.cn/api/paas/v4，可换 Groq 等兼容服务。
    pub asr_base_url: Option<String>,
    /// ASR 模型名，默认 glm-asr-2512。
    pub asr_model: Option<String>,
    /// 切片时长（秒），默认 600；转写按片顺序执行后拼接。
    pub slice_secs: Option<u64>,
    /// 工作目录（原始文件/切片落盘处），默认 /data/lecture。
    pub workspace: Option<String>,
    /// Vault 内课程根目录（相对路径），默认 01-Courses-课程。
    /// 转写存 `{vault_dir}/{课程}/转写/`，整理笔记存 `{vault_dir}/{课程}/笔记/`。
    pub vault_dir: Option<String>,
    /// 未匹配到当天课时，转写归入的课程名，默认 课堂录音。
    pub fallback_course: Option<String>,
}

impl LectureSection {
    pub const DEFAULT_API_KEY_ENV: &str = "ZHIPU_API_KEY";
    pub const DEFAULT_ASR_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
    pub const DEFAULT_ASR_MODEL: &str = "glm-asr-2512";
    pub const DEFAULT_SLICE_SECS: u64 = 600;
    pub const DEFAULT_WORKSPACE: &str = "/data/lecture";
    pub const DEFAULT_VAULT_DIR: &str = "01-Courses-课程";
    pub const DEFAULT_FALLBACK_COURSE: &str = "课堂录音";
    /// worker 轮询间隔（秒）。
    pub const DEFAULT_INTERVAL_SECS: u64 = 60;

    pub fn is_enabled(&self) -> bool {
        self.enabled != Some(false)
    }

    /// API key 已可解析（明文或环境变量）即认为模块就绪。
    pub fn is_ready(&self) -> bool {
        if !self.is_enabled() {
            return false;
        }
        if self
            .api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty())
        {
            return true;
        }
        let name = self
            .api_key_env
            .clone()
            .unwrap_or_else(|| Self::DEFAULT_API_KEY_ENV.into());
        std::env::var(&name).is_ok_and(|v| !v.trim().is_empty())
    }

    /// 未就绪的原因（schedule list 展示）。
    pub fn not_ready_reason(&self) -> Option<String> {
        if !self.is_enabled() {
            return Some("sources.lecture.enabled = false".into());
        }
        let name = self
            .api_key_env
            .clone()
            .unwrap_or_else(|| Self::DEFAULT_API_KEY_ENV.into());
        (!self.is_ready()).then(|| {
            format!("缺 ASR key：配置 sources.lecture.api_key 或环境变量 {name}")
        })
    }

    pub fn resolved(&self) -> Result<LectureConfig> {
        let api_key = match &self.api_key {
            Some(k) if !k.trim().is_empty() => k.trim().to_owned(),
            _ => {
                let name = self
                    .api_key_env
                    .clone()
                    .unwrap_or_else(|| Self::DEFAULT_API_KEY_ENV.into());
                std::env::var(&name)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .with_context(|| format!("sources.lecture 缺 ASR key（api_key 或 {name}）"))?
            }
        };
        let workspace = self
            .workspace
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| Self::DEFAULT_WORKSPACE.into());
        Ok(LectureConfig {
            api_key,
            base_url: self
                .asr_base_url
                .clone()
                .filter(|u| !u.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_ASR_BASE_URL.into())
                .trim_end_matches('/')
                .to_owned(),
            model: self
                .asr_model
                .clone()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_ASR_MODEL.into()),
            slice_secs: self
                .slice_secs
                .filter(|s| *s >= 30)
                .unwrap_or(Self::DEFAULT_SLICE_SECS),
            workspace: workspace.into(),
            vault_dir: self
                .vault_dir
                .clone()
                .filter(|d| !d.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_VAULT_DIR.into()),
            fallback_course: self
                .fallback_course
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_FALLBACK_COURSE.into()),
        })
    }
}

/// 运行时配置；默认值只允许在 LectureSection::resolved 里出现。
#[derive(Debug, Clone)]
pub struct LectureConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub slice_secs: u64,
    pub workspace: std::path::PathBuf,
    pub vault_dir: String,
    pub fallback_course: String,
}
