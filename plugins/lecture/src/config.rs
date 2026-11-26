//! 课堂录音附属配置：`[sources.lecture]`。
//! ASR 走 OpenAI 兼容 `/audio/transcriptions`（默认智谱 GLM-ASR），
//! key 优先取 `api_key_env` 环境变量，配置里的明文 `api_key` 只作兜底。

use anyhow::Result;
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
    /// ASR 接口风格：`openai`（默认，OpenAI 兼容 /audio/transcriptions）
    /// 或 `whispercpp`（whisper.cpp server 原生 /inference）。
    pub asr_api: Option<String>,
    /// ASR base URL（OpenAI 兼容时不含 /audio/transcriptions）。
    /// 默认 https://open.bigmodel.cn/api/paas/v4，可换 Groq 等兼容服务。
    pub asr_base_url: Option<String>,
    /// 整理笔记用的 agent 底座（codex / pi）。默认 pi；
    /// 供应方限流时（如智谱 429）改用 pi 更稳。
    pub backend: Option<String>,
    /// ASR 模型名，默认 glm-asr-2512。
    pub asr_model: Option<String>,
    /// 切片时长（秒），默认 600；转写按片顺序执行后拼接。
    pub slice_secs: Option<u64>,
    /// 工作目录（原始文件/切片落盘处），默认 /data/lecture。
    pub workspace: Option<String>,
    /// Vault 内课程根目录（相对路径），默认 01-Courses-课程；
    /// 整理笔记存 `{vault_dir}/{课程}/课堂笔记.md`（转写是临时产物，不入 Vault）。
    pub vault_dir: Option<String>,
    /// Vault 在 agent 沙箱里的绝对路径（容器内挂载点），默认 /vault。
    /// 笔记必须写这个绝对路径：相对路径会落进任务工作目录（沙箱），写完就丢。
    pub vault_root: Option<String>,
    /// 课程认不出来时，笔记暂存到 Vault 的哪个目录（相对 vault_root），默认 00-Inbox-收件箱。
    pub inbox_dir: Option<String>,
    /// 未匹配到当天课时，转写归入的课程名，默认 课堂录音。
    pub fallback_course: Option<String>,
    /// 原始录音保留天数（转写完成后不立刻删，过期再清）。默认 7 天，0 = 立刻删。
    pub retain_audio_days: Option<u64>,
    /// 用哪个转写能力。默认 `tingwu`（通义听悟：云端、带说话人分离），
    /// 它失败时自动回落到本地 ASR。设成 `local` 就只用本地 ASR
    /// （不碰云端、不耗听悟额度）。
    pub transcriber: Option<String>,
    /// 听悟 cookie 在凭据托管库里的名字（默认 `tingwu-cookie`）。
    pub tingwu_credential: Option<String>,
}

impl LectureSection {
    pub const DEFAULT_API_KEY_ENV: &str = "ZHIPU_API_KEY";
    pub const DEFAULT_ASR_API: &str = "openai";
    pub const DEFAULT_ASR_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
    pub const DEFAULT_ASR_MODEL: &str = "glm-asr-2512";
    pub const DEFAULT_SLICE_SECS: u64 = 600;
    /// 原始录音默认保留 7 天（转写产出后不立刻删，转写有疑问还能重跑 ASR）。
    pub const DEFAULT_RETAIN_AUDIO_DAYS: u64 = 7;
    /// 默认用通义听悟转写（云端、带说话人分离；失败自动回落本地 ASR）。
    pub const DEFAULT_TRANSCRIBER: &str = "tingwu";
    /// 听悟 cookie 在凭据托管库里的默认名字。
    pub const DEFAULT_TINGWU_CREDENTIAL: &str = "tingwu-cookie";
    pub const DEFAULT_WORKSPACE: &str = "/data/lecture";
    pub const DEFAULT_VAULT_DIR: &str = "01-Courses-课程";
    pub const DEFAULT_VAULT_ROOT: &str = "/vault";
    pub const DEFAULT_INBOX_DIR: &str = "00-Inbox-收件箱";
    /// 整理笔记默认底座（codex / pi）。
    pub const DEFAULT_BACKEND: &str = "pi";
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
        (!self.is_ready())
            .then(|| format!("缺 ASR key：配置 sources.lecture.api_key 或环境变量 {name}"))
    }

    pub fn resolved(&self) -> Result<LectureConfig> {
        // ASR key：**环境变量优先**，配置里的明文 `api_key` 只作兜底。
        // 配置文件会挂进 worker（有 agent 的容器），明文 key 放文件里等于交给 agent 读。
        //
        // ⚠️ 缺 key **不报错**，解析成空串：ASR key 只有转写那一步要，
        // 而网关侧（QQ/Telegram）只负责「下载 + 登记待办」，压根不调 ASR。
        // 之前在这里直接报错 → 网关连 handler 都注册不上 → 录音没人接管，
        // 消息掉给 agent，agent 回「请补充具体处理要求」而不是自动转写
        // （2026-11-17 踩过）。worker 侧的真就绪由 tracker 的 ensure!(is_ready()) 管。
        let api_key = {
            let name = self
                .api_key_env
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_API_KEY_ENV.into());
            match std::env::var(&name) {
                Ok(value) if !value.trim().is_empty() => value.trim().to_owned(),
                _ => self
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_default()
                    .to_owned(),
            }
        };
        let workspace = self
            .workspace
            .clone()
            .filter(|p| !p.trim().is_empty())
            .unwrap_or_else(|| Self::DEFAULT_WORKSPACE.into());
        Ok(LectureConfig {
            api_key,
            api: self
                .asr_api
                .clone()
                .filter(|a| !a.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_ASR_API.into()),
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
            vault_root: self
                .vault_root
                .clone()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_VAULT_ROOT.into()),
            inbox_dir: self
                .inbox_dir
                .clone()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_INBOX_DIR.into()),
            retain_audio_days: self
                .retain_audio_days
                .unwrap_or(Self::DEFAULT_RETAIN_AUDIO_DAYS),
            transcriber: self
                .transcriber
                .clone()
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| Self::DEFAULT_TRANSCRIBER.into()),
            tingwu_credential: self
                .tingwu_credential
                .clone()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| Self::DEFAULT_TINGWU_CREDENTIAL.into()),
            backend: self
                .backend
                .clone()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| Self::DEFAULT_BACKEND.into()),
        })
    }
}

/// 运行时配置；默认值只允许在 LectureSection::resolved 里出现。
#[derive(Debug, Clone)]
pub struct LectureConfig {
    pub api_key: String,
    pub api: String,
    pub base_url: String,
    pub model: String,
    pub slice_secs: u64,
    pub workspace: std::path::PathBuf,
    pub vault_dir: String,
    pub vault_root: String,
    pub inbox_dir: String,
    pub fallback_course: String,
    /// 整理笔记用的 agent 底座（codex / pi）。
    pub backend: String,
    /// 原始录音保留天数：切片立刻删（它是磁盘大头），原始音频留一段时间，
    /// 这样转写有疑问还能重新跑 ASR。0 表示转写完立刻删原始音频。
    pub retain_audio_days: u64,
    /// 优先用哪个转写能力；`local` = 只用本地 ASR。
    pub transcriber: String,
    /// 听悟 cookie 在凭据托管库里的名字。
    pub tingwu_credential: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 网关侧（QQ/Telegram）**没有** ASR key 也得能解析出配置：
    /// 那边的 handler 只负责「下载 + 登记待办」，不调 ASR。
    ///
    /// 之前这里缺 key 直接报错 → 网关的 handler 注册不上 → 录音没人接管，
    /// 消息掉给 agent（2026-11-17 踩过）。
    #[test]
    fn resolves_without_the_asr_key() {
        // 用一个几乎不可能存在的环境变量名，确保解析时取不到 key。
        let section = LectureSection {
            api_key: None,
            api_key_env: Some("RU41_LECTURE_ASR_KEY_THAT_DOES_NOT_EXIST".to_owned()),
            ..Default::default()
        };
        let config = section.resolved().expect("网关侧不该因为缺 ASR key 而失败");
        assert!(config.api_key.is_empty(), "{}", config.api_key);
    }

    /// key 在环境里时照常取到（转写那一步要的就是它）。
    #[test]
    fn picks_the_asr_key_from_the_environment() {
        let section = LectureSection {
            api_key: None,
            api_key_env: Some("RU41_LECTURE_ASR_KEY".to_owned()),
            ..Default::default()
        };
        // 没设这个变量时不该崩；设了就能取到（这里只验证不报错 + 空值兜底）。
        let config = section.resolved().expect("解析不应失败");
        assert!(!config.api_key.contains('\n'));
    }
}
