//! Vault rebase 冲突的 LLM 语义合并。
//!
//! 笔记仓库的冲突几乎全是「两边各加了几行」这类语义并集，git 三路合并
//! 处理不了就只剩人肉。这里把带冲突标记的文件全文交给 chat 模型合并，
//! 输出经严格校验（无残留标记、非空、不暴长）才落盘；任何一步失败由
//! 调用方 abort rebase 并通知人工——AI 合并只是省事，不是可信路径。

use anyhow::{Context, Result, bail};
use std::time::Duration;

/// AI 合并的运行时设置（obsidian.merge_* 配置 + key 解析后的产物）。
#[derive(Debug, Clone)]
pub struct MergeSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

/// chat completions 风格的冲突合并器（默认 GLM coding 端点）。
pub struct MergeResolver {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl MergeResolver {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("merge http client"),
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            api_key: api_key.into(),
        }
    }

    /// 合并单个带冲突标记的文件，返回合并后的完整内容。
    pub async fn resolve(&self, path: &str, conflicted: &str) -> Result<String> {
        let body = serde_json::json!({
            "model": self.model,
            "temperature": 0.2,
            "messages": [{"role": "user", "content": build_prompt(path, conflicted)}],
        });
        let response = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("合并请求失败")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("合并 HTTP {status}：{}", &text[..text.len().min(200)]);
        }
        let raw = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| {
                v.pointer("/choices/0/message/content")
                    .and_then(|c| c.as_str())
                    .map(str::to_owned)
            })
            .context("合并响应缺 choices[0].message.content")?;
        let merged = clean_response(&raw);
        validate(&merged, conflicted)?;
        Ok(merged)
    }
}

fn build_prompt(path: &str, conflicted: &str) -> String {
    format!(
        "你是 git 合并助手。文件 `{path}` 在 rebase 时产生冲突，内容含冲突标记 \
         （<<<<<<< HEAD 是本地提交一侧，======= 分隔，>>>>>>> 是远端一侧）。\n\
         请语义合并：两侧内容取并集、去重、保持 markdown 结构连贯；两侧做了相同修改就只保留一份。\n\
         只输出合并后的完整文件内容，不要解释，不要用代码块包裹。\n\n{conflicted}"
    )
}

/// 去掉模型可能裹的代码围栏；只接受「整个回复就是一个围栏块」的剥壳。
fn clean_response(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed
        .strip_prefix("```")
        .and_then(|r| r.strip_suffix("```"))
    {
        // 开围栏行带语言标记（```markdown）则剥掉首行；裸围栏（```\n）首行即内容。
        let body = if rest.starts_with('\n') {
            &rest[1..]
        } else {
            match rest.split_once('\n') {
                Some((_info, body)) => body,
                None => rest,
            }
        };
        return body.trim_end().to_owned();
    }
    trimmed.to_owned()
}

/// 合并结果校验：无残留冲突标记、非空、不超过原文 3 倍（防模型复读暴走）。
fn validate(merged: &str, original: &str) -> Result<()> {
    if merged.trim().is_empty() {
        bail!("合并结果为空");
    }
    // 冲突标记是独占一行的 7 字符序列；setext 下划线等短串不误伤。
    for line in merged.lines() {
        let t = line.trim_end();
        if t.starts_with("<<<<<<<") || t == "=======" || t.starts_with(">>>>>>>") {
            bail!("合并结果残留冲突标记行 {t:?}");
        }
    }
    if merged.len() > original.len().saturating_mul(3) + 1024 {
        bail!("合并结果异常膨胀（{} -> {} 字节）", original.len(), merged.len());
    }
    Ok(())
}

/// 只允许文本类文件走 AI 合并；二进制/其他类型直接交人工。
pub fn is_mergeable_path(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    matches!(ext.as_str(), "md" | "markdown" | "txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_strips_fenced_block() {
        assert_eq!(clean_response("```markdown\n# 标题\n内容\n```"), "# 标题\n内容");
        assert_eq!(clean_response("# 直接内容"), "# 直接内容");
        assert_eq!(clean_response("```\n内容\n```"), "内容");
    }

    #[test]
    fn validate_rejects_leftover_markers() {
        assert!(validate("好的内容", "原文").is_ok());
        assert!(validate("<<<<<<< HEAD\n内容", "原文").is_err());
        assert!(validate("", "原文").is_err());
        assert!(validate(&"长".repeat(10000), "短").is_err());
    }

    #[test]
    fn mergeable_only_text() {
        assert!(is_mergeable_path("笔记/作业总表.md"));
        assert!(is_mergeable_path("a.txt"));
        assert!(!is_mergeable_path("图.png"));
        assert!(!is_mergeable_path("无扩展名"));
    }
}
