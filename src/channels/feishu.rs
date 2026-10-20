use crate::ports::channel::NotifyChannel;
use anyhow::{Context, Result, bail};
use std::{future::Future, pin::Pin};

/// 通知 target：使用配置中默认 webhook 的占位值，避免把带 token 的 URL 落库。
pub const DEFAULT_TARGET: &str = "default";

/// 飞书自定义群机器人 webhook：纯主动推送出口，无被动回复与双向交互。
/// 安全校验依赖机器人侧的关键词或 IP 白名单设置；签名验证暂未支持。
// TODO: 支持飞书 webhook 签名验证（需引入 hmac/sha2/base64 依赖，先评估是否值得）。
pub struct FeishuWebhookNotifier {
    http: reqwest::Client,
    /// target 为 default 时使用的 webhook URL。
    default_url: String,
}

impl FeishuWebhookNotifier {
    pub fn new(default_url: impl Into<String>) -> Self {
        Self {
            http: crate::util::http_client(),
            default_url: default_url.into(),
        }
    }

    /// 解析 target：default 用配置 URL，其余必须是完整 webhook URL。
    fn resolve<'a>(&'a self, target: &'a str) -> Result<&'a str> {
        if target == DEFAULT_TARGET {
            return Ok(&self.default_url);
        }
        if target.starts_with("https://") || target.starts_with("http://") {
            return Ok(target);
        }
        bail!("unknown feishu notify target: {target}");
    }
}

impl NotifyChannel for FeishuWebhookNotifier {
    fn channel_id(&self) -> &str {
        "feishu"
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        _msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let url = self.resolve(target)?;
            let response = self
                .http
                .post(url)
                .json(&serde_json::json!({
                    "msg_type": "text",
                    "content": { "text": text },
                }))
                .send()
                .await
                .context("feishu webhook request failed")?;
            let body: serde_json::Value = response
                .json()
                .await
                .context("feishu webhook returned invalid json")?;
            // 新版返回 code/msg，旧版返回 StatusCode/StatusMessage，两种都接受。
            let code = body
                .get("code")
                .or_else(|| body.get("StatusCode"))
                .and_then(|v| v.as_i64())
                .context("feishu webhook response missing code")?;
            if code != 0 {
                bail!("feishu webhook rejected message: {body}");
            }
            Ok(())
        })
    }
}
