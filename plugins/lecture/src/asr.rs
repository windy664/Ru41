//! ASR 客户端：支持两种接口风格——
//! `openai`：OpenAI 兼容 `/audio/transcriptions`（默认，智谱 GLM-ASR / Groq 等）；
//! `whispercpp`：whisper.cpp server 原生 `/inference`（本地免费 ASR）。
//! 换服务商只改配置，不动代码。

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsrApi {
    OpenAi,
    WhisperCpp,
}

impl AsrApi {
    fn parse(value: &str) -> Result<Self> {
        match value.trim() {
            "openai" => Ok(Self::OpenAi),
            "whispercpp" => Ok(Self::WhisperCpp),
            other => anyhow::bail!("sources.lecture.asr_api 只支持 openai / whispercpp，收到 {other:?}"),
        }
    }
}

pub struct AsrClient {
    http: reqwest::Client,
    api: AsrApi,
    base_url: String,
    api_key: String,
    model: String,
}

impl AsrClient {
    pub fn new(base_url: &str, api_key: &str, model: &str, api: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("asr http client"),
            api: AsrApi::parse(api)?,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
        })
    }

    fn endpoint(&self) -> String {
        match self.api {
            AsrApi::OpenAi => format!("{}/audio/transcriptions", self.base_url),
            AsrApi::WhisperCpp => format!("{}/inference", self.base_url),
        }
    }

    fn form(&self, part: reqwest::multipart::Part) -> reqwest::multipart::Form {
        match self.api {
            AsrApi::OpenAi => reqwest::multipart::Form::new()
                .text("model", self.model.clone())
                .text("stream", "false")
                .part("file", part),
            AsrApi::WhisperCpp => reqwest::multipart::Form::new()
                .text("response-format", "json")
                .part("file", part),
        }
    }

    /// 转写一个音频文件；网络/5xx 自动重试，4xx 直接报错（key/参数问题重试无意义）。
    pub async fn transcribe(&self, path: &Path) -> Result<String> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.mp3")
            .to_owned();
        let bytes =
            std::fs::read(path).with_context(|| format!("读取切片失败 {}", path.display()))?;
        let mut last_error = None;
        for attempt in 1..=3 {
            // multipart body 一次性消费，重试时重建。
            let part = reqwest::multipart::Part::bytes(bytes.clone())
                .file_name(file_name.clone())
                .mime_str("audio/mpeg")?;
            let form = self.form(part);
            let request = self.http.post(self.endpoint()).multipart(form);
            // whisper.cpp 本地服务不鉴权；OpenAI 兼容服务带 Bearer。
            let request = match self.api {
                AsrApi::OpenAi => request.bearer_auth(&self.api_key),
                AsrApi::WhisperCpp => request,
            };
            let response = match request.send().await {
                Ok(r) => r,
                Err(error) => {
                    last_error = Some(anyhow::anyhow!("ASR 请求失败（第 {attempt} 次）：{error}"));
                    continue;
                }
            };
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if status.is_success() {
                let text: String = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.get("text")
                            .and_then(|t| t.as_str())
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .map(str::to_owned)
                    })
                    .with_context(|| format!("ASR 响应缺少 text 字段：{}", truncate(&body)))?;
                return Ok(text);
            }
            let error = anyhow::anyhow!("ASR HTTP {status}（第 {attempt} 次）：{}", truncate(&body));
            if status.as_u16() < 500 {
                return Err(error);
            }
            last_error = Some(error);
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("ASR 未知错误")))
    }
}

fn truncate(body: &str) -> String {
    let mut line = body.lines().next().unwrap_or_default().to_owned();
    if line.chars().count() > 200 {
        line = line.chars().take(200).collect();
        line.push('…');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_first_line_bounded() {
        assert_eq!(truncate("a\nb"), "a");
        assert_eq!(truncate(&"x".repeat(300)).chars().count(), 201);
    }

    #[test]
    fn asr_api_parse() {
        assert_eq!(AsrApi::parse("openai").unwrap(), AsrApi::OpenAi);
        assert_eq!(AsrApi::parse("whispercpp").unwrap(), AsrApi::WhisperCpp);
        assert!(AsrApi::parse("groq").is_err());
    }

    #[test]
    fn endpoint_per_api_style() {
        let openai = AsrClient::new("http://asr:8080/v1", "k", "m", "openai").unwrap();
        assert_eq!(openai.endpoint(), "http://asr:8080/v1/audio/transcriptions");
        let local = AsrClient::new("http://asr:8080", "k", "m", "whispercpp").unwrap();
        assert_eq!(local.endpoint(), "http://asr:8080/inference");
    }
}
