//! Telegram Bot API 客户端：只封装 Ru41 用到的方法，不做框架化抽象。
//!
//! 协议要点（官方文档 <https://core.telegram.org/bots/api>）：
//! - 请求形式固定为 `https://api.telegram.org/bot<token>/METHOD_NAME`；
//! - 响应恒为 `{"ok": bool, "result": ..., "description": ..., "error_code": int}`；
//! - **token 出现在 URL 路径里**，因此日志与错误信息只允许出现脱敏后的地址。

use crate::ports::channel::{MediaKind, MediaRef};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// Bot API 根地址；测试用本地 mock server 覆盖。
const DEFAULT_BASE: &str = "https://api.telegram.org";

/// bot 身份：`getMe` 的结果子集。
#[derive(Debug, Clone, Default)]
pub struct BotIdentity {
    pub id: i64,
    pub username: String,
    pub first_name: String,
}

/// Telegram Bot API 客户端。
pub struct TelegramApi {
    http: reqwest::Client,
    token: String,
    base: String,
}

impl TelegramApi {
    pub fn new(token: impl Into<String>) -> Self {
        Self::build(token, None)
    }

    /// 带专属出网代理的客户端：Telegram 在境内不可直连，代理只作用于本渠道，
    /// 不影响进程内其他流量（agent 调用、其它渠道）。
    pub fn with_proxy(token: impl Into<String>, proxy_url: Option<&str>) -> Self {
        Self::build(token, proxy_url)
    }

    fn build(token: impl Into<String>, proxy_url: Option<&str>) -> Self {
        let mut builder = crate::util::http_client_builder()
            // 长轮询的服务端等待最长 20s，客户端总超时必须更长，否则会被自己掐断。
            .timeout(std::time::Duration::from_secs(90));
        if let Some(url) = proxy_url.map(str::trim).filter(|url| !url.is_empty()) {
            match reqwest::Proxy::all(url) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(error) => {
                    eprintln!("[telegram] 代理地址无效（已忽略）：{url} ({error})");
                }
            }
        }
        Self {
            http: builder.build().expect("reqwest client build"),
            token: token.into(),
            base: DEFAULT_BASE.to_owned(),
        }
    }

    /// 测试用：把 API 根地址换成本地 mock server。
    pub fn with_base(token: impl Into<String>, base: impl Into<String>) -> Self {
        Self {
            http: crate::util::http_client(),
            token: token.into(),
            base: base.into(),
        }
    }

    /// 脱敏地址：日志与错误里只允许出现它。
    pub fn redacted_url(&self, method: &str) -> String {
        format!("{}/bot<redacted>/{method}", self.base)
    }

    fn url(&self, method: &str) -> String {
        format!("{}/bot{}/{method}", self.base, self.token)
    }

    /// 把 token 从任意文本里抹掉（平台偶尔会在 description 里回显请求地址）。
    fn sanitize(&self, text: &str) -> String {
        if self.token.is_empty() {
            text.to_owned()
        } else {
            text.replace(&self.token, "<redacted>")
        }
    }

    /// 调用一个 Bot API 方法；`ok=false` 视为失败并带上 description。
    pub async fn call(&self, method: &str, body: Value) -> Result<Value> {
        let response = self
            .http
            .post(self.url(method))
            .json(&body)
            .send()
            .await
            .with_context(|| format!("telegram {} request failed", self.redacted_url(method)))?;
        self.finish(method, response).await
    }

    /// multipart 上传（本地文件发媒体时用）。
    async fn call_multipart(&self, method: &str, form: reqwest::multipart::Form) -> Result<Value> {
        let response = self
            .http
            .post(self.url(method))
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("telegram {} request failed", self.redacted_url(method)))?;
        self.finish(method, response).await
    }

    /// 解析平台响应：成功取 `result`，失败把 description 抛成错误。
    async fn finish(&self, method: &str, response: reqwest::Response) -> Result<Value> {
        let payload: Value = response.json().await.with_context(|| {
            format!(
                "telegram {} returned invalid json",
                self.redacted_url(method)
            )
        })?;
        let ok = payload.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if !ok {
            let description = payload
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            let code = payload
                .get("error_code")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            bail!(
                "telegram {} rejected ({code}): {}",
                self.redacted_url(method),
                self.sanitize(description)
            );
        }
        Ok(payload.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 自检：bot 身份。用于 `ru41 telegram doctor`。
    pub async fn get_me(&self) -> Result<BotIdentity> {
        let result = self.call("getMe", json!({})).await?;
        Ok(BotIdentity {
            id: result.get("id").and_then(Value::as_i64).unwrap_or_default(),
            username: result
                .get("username")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            first_name: result
                .get("first_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// 长轮询取更新：offset = 已处理 update_id + 1。
    pub async fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Value>> {
        let result = self
            .call(
                "getUpdates",
                json!({
                    "offset": offset,
                    "timeout": timeout_secs,
                    "allowed_updates": ["message", "callback_query"],
                }),
            )
            .await?;
        Ok(result.as_array().cloned().unwrap_or_default())
    }

    /// 平台侧待处理条数：长轮询存活探针用（进程活着但队列不消 → 卡死）。
    pub async fn pending_update_count(&self) -> Result<i64> {
        let result = self.call("getWebhookInfo", json!({})).await?;
        Ok(result["pending_update_count"].as_i64().unwrap_or(0))
    }

    /// 按钮回调应答（必须及时，否则客户端一直转圈）。
    pub async fn answer_callback_query(&self, callback_id: &str) -> Result<Value> {
        self.call(
            "answerCallbackQuery",
            json!({"callback_query_id": callback_id}),
        )
        .await
    }

    /// 发文本消息。`parse_mode` 传 `HTML` 用排版，传 `None` 发纯文本；
    /// `reply_to` 是被回复消息的 message_id（Telegram 没有 QQ 那样的被动回复窗口，
    /// 这个参数可以长期有效）。
    pub async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to: Option<i64>,
        reply_markup: Option<&Value>,
    ) -> Result<Value> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "link_preview_options": {"is_disabled": true},
        });
        if let Some(mode) = parse_mode {
            body["parse_mode"] = json!(mode);
        }
        if let Some(message_id) = reply_to {
            body["reply_parameters"] = json!({"message_id": message_id});
        }
        if let Some(markup) = reply_markup {
            body["reply_markup"] = markup.clone();
        }
        self.call("sendMessage", body).await
    }

    /// 发媒体：远端 URL 直接引用，本地文件走 multipart 上传。
    pub async fn send_media(
        &self,
        chat_id: &str,
        media: &MediaRef,
        caption: &str,
    ) -> Result<Value> {
        let (method, field) = match media.kind {
            MediaKind::Image => ("sendPhoto", "photo"),
            MediaKind::Video => ("sendVideo", "video"),
            MediaKind::Voice => ("sendVoice", "voice"),
            MediaKind::File => ("sendDocument", "document"),
        };
        if media.is_remote() {
            let mut body = json!({"chat_id": chat_id});
            body[field] = json!(media.source);
            if !caption.is_empty() {
                body["caption"] = json!(caption);
            }
            return self.call(method, body).await;
        }
        let bytes = std::fs::read(&media.source)
            .with_context(|| format!("cannot read media file: {}", media.display_name()))?;
        let part =
            reqwest::multipart::Part::bytes(bytes).file_name(media.display_name().to_owned());
        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_owned())
            .part(field, part);
        if !caption.is_empty() {
            form = form.text("caption", caption.to_owned());
        }
        self.call_multipart(method, form).await
    }
}

/// 平台是否因为排版解析失败而拒收（可降级成纯文本重发）。
pub fn is_parse_error(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("can't parse entities")
        || text.contains("Unsupported start tag")
        || text.contains("can't find end tag")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacted_url_never_contains_token() {
        let api = TelegramApi::new("123456:secret-token-value");
        let url = api.redacted_url("sendMessage");
        assert!(!url.contains("secret-token-value"));
        assert!(url.contains("<redacted>"));
        assert!(url.ends_with("/sendMessage"));
    }

    #[test]
    fn sanitize_replaces_token_in_platform_text() {
        let api = TelegramApi::new("123456:secret-token-value");
        let cleaned = api.sanitize("failed for bot123456:secret-token-value");
        assert_eq!(cleaned, "failed for bot<redacted>");
    }

    #[test]
    fn parse_error_detection_covers_platform_wording() {
        let ok = anyhow::anyhow!("telegram rejected (400): Bad Request: chat not found");
        assert!(!is_parse_error(&ok));
        let bad = anyhow::anyhow!("telegram rejected (400): Bad Request: can't parse entities");
        assert!(is_parse_error(&bad));
    }
}
