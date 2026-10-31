//! ASR 客户端：OpenAI 兼容 `/audio/transcriptions`（multipart 上传，异步）。
//! 默认指向智谱 GLM-ASR；换 Groq 等兼容服务只改 base_url/model/key。

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

pub struct AsrClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AsrClient {
    pub fn new(base_url: &str, api_key: &str, model: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(180))
                .build()
                .expect("asr http client"),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
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
            let form = reqwest::multipart::Form::new()
                .text("model", self.model.clone())
                .text("stream", "false")
                .part("file", part);
            let response = match self
                .http
                .post(format!("{}/audio/transcriptions", self.base_url))
                .bearer_auth(&self.api_key)
                .multipart(form)
                .send()
                .await
            {
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
}
