use anyhow::{Context, Result, bail};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const DEFAULT_API_BASE: &str = "https://api.sgroup.qq.com";
const DEFAULT_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";

struct CachedToken {
    value: String,
    expire_at: Instant,
}

/// QQ 开放平台 REST 客户端：凭证缓存刷新、被动回复与主动消息。
pub struct QqApiClient {
    http: reqwest::Client,
    app_id: String,
    client_secret: String,
    api_base: String,
    token_url: String,
    token: Mutex<Option<CachedToken>>,
}

impl QqApiClient {
    pub fn new(app_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self::with_endpoints(
            app_id,
            client_secret,
            DEFAULT_API_BASE.into(),
            DEFAULT_TOKEN_URL.into(),
        )
    }

    pub fn with_endpoints(
        app_id: impl Into<String>,
        client_secret: impl Into<String>,
        api_base: String,
        token_url: String,
    ) -> Self {
        Self {
            http: crate::util::http_client(),
            app_id: app_id.into(),
            client_secret: client_secret.into(),
            api_base,
            token_url,
            token: Mutex::new(None),
        }
    }

    /// 取有效 access_token；提前 60 秒视为过期并重新申请。
    pub async fn access_token(&self) -> Result<String> {
        let mut cache = self.token.lock().await;
        if let Some(cached) = cache.as_ref()
            && Instant::now() < cached.expire_at
        {
            return Ok(cached.value.clone());
        }
        let response = self
            .http
            .post(&self.token_url)
            .json(&serde_json::json!({
                "appId": self.app_id,
                "clientSecret": self.client_secret,
            }))
            .send()
            .await
            .context("cannot request QQ access token")?;
        let status = response.status();
        let body: serde_json::Value = response
            .json()
            .await
            .context("invalid QQ token response JSON")?;
        if !status.is_success() {
            bail!("QQ token request failed with {status}: {body}");
        }
        let token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .context("QQ token response missing access_token")?
            .to_owned();
        // expires_in 可能是字符串或数字。
        let expires_in = body
            .get("expires_in")
            .and_then(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(7200);
        *cache = Some(CachedToken {
            value: token.clone(),
            expire_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
        });
        Ok(token)
    }

    /// GET /gateway 拿 WebSocket 地址。
    pub async fn gateway_url(&self) -> Result<String> {
        let body: serde_json::Value = self
            .authorized(self.http.get(format!("{}/gateway", self.api_base)))
            .await?
            .send()
            .await
            .context("cannot request QQ gateway URL")?
            .error_for_status()
            .context("QQ gateway URL request rejected")?
            .json()
            .await?;
        body.get("url")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .context("QQ gateway response missing url")
    }

    /// 被动回复群消息；msg_id 来自事件，5 分钟内有效。
    pub async fn send_group_text(
        &self,
        group_openid: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        self.post_message(
            &format!("{}/v2/groups/{group_openid}/messages", self.api_base),
            content,
            None,
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 被动回复群消息，附带快捷按钮（markdown + keyboard）。
    pub async fn send_group_markup(
        &self,
        group_openid: &str,
        content: &str,
        markup: &crate::ports::commands::ReplyMarkup,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        self.post_message(
            &format!("{}/v2/groups/{group_openid}/messages", self.api_base),
            content,
            Some(markup),
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 被动回复单聊（C2C）消息。
    pub async fn send_c2c_text(
        &self,
        user_openid: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        self.post_message(
            &format!("{}/v2/users/{user_openid}/messages", self.api_base),
            content,
            None,
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 被动回复单聊消息，附带快捷按钮（markdown + keyboard）。
    pub async fn send_c2c_markup(
        &self,
        user_openid: &str,
        content: &str,
        markup: &crate::ports::commands::ReplyMarkup,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        self.post_message(
            &format!("{}/v2/users/{user_openid}/messages", self.api_base),
            content,
            Some(markup),
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 单聊发送富媒体（msg_type 7）：先上传拿 file_info，再发消息。
    pub async fn send_c2c_media(
        &self,
        user_openid: &str,
        caption: &str,
        media: &crate::ports::channel::MediaRef,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        let file_info = self.upload_media("users", user_openid, media).await?;
        self.post_media(
            &format!("{}/v2/users/{user_openid}/messages", self.api_base),
            caption,
            &file_info,
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 群聊发送富媒体（msg_type 7）。
    pub async fn send_group_media(
        &self,
        group_openid: &str,
        caption: &str,
        media: &crate::ports::channel::MediaRef,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        let file_info = self.upload_media("groups", group_openid, media).await?;
        self.post_media(
            &format!("{}/v2/groups/{group_openid}/messages", self.api_base),
            caption,
            &file_info,
            msg_id,
            msg_seq,
        )
        .await
    }

    /// 富媒体上传：URL 直接引用；本地文件读字节 base64 上传。
    /// file_type: 1 图片 / 2 视频 / 3 语音 / 4 文件。返回用于发消息的 file_info。
    async fn upload_media(
        &self,
        scope: &str,
        openid: &str,
        media: &crate::ports::channel::MediaRef,
    ) -> Result<String> {
        use crate::ports::channel::MediaKind;
        let file_type = match media.kind {
            MediaKind::Image => 1,
            MediaKind::Video => 2,
            MediaKind::Voice => 3,
            MediaKind::File => 4,
        };
        let mut body = serde_json::json!({
            "file_type": file_type,
            "srv_send_msg": false,
        });
        if media.is_remote() {
            body["url"] = serde_json::Value::String(media.source.clone());
        } else {
            let bytes = tokio::fs::read(&media.source)
                .await
                .with_context(|| format!("cannot read media file {}", media.source))?;
            use base64::Engine;
            body["file_data"] =
                serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(bytes));
            body["file_name"] = serde_json::Value::String(media.display_name().to_owned());
        }
        let response = self
            .authorized(
                self.http
                    .post(format!("{}/v2/{scope}/{openid}/files", self.api_base)),
            )
            .await?
            .json(&body)
            .send()
            .await
            .context("cannot upload QQ media")?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("QQ media upload failed with {status}: {body}");
        }
        body.get("file_info")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .with_context(|| format!("QQ media upload response missing file_info: {body}"))
    }

    async fn post_media(
        &self,
        url: &str,
        caption: &str,
        file_info: &str,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        let body = media_body(caption, file_info, msg_id, msg_seq);
        let response = self
            .authorized(self.http.post(url))
            .await?
            .json(&body)
            .send()
            .await
            .context("cannot send QQ media message")?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        eprintln!(
            "[qq.send] {} media=true status={status} err={}",
            url.rsplit('/').nth(2).unwrap_or(url),
            body.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
        );
        if !status.is_success() {
            bail!("QQ send media failed with {status}: {body}");
        }
        Ok(body)
    }

    async fn post_message(
        &self,
        url: &str,
        content: &str,
        markup: Option<&crate::ports::commands::ReplyMarkup>,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        // 键盘只有 markdown 消息支持：带键盘 → 纯 markdown → 纯文本三级降级。
        let keyboard = markup.and_then(qq_keyboard);
        let attempts: Vec<(bool, Option<serde_json::Value>)> = match keyboard {
            Some(keyboard) => vec![(true, Some(keyboard)), (true, None), (false, None)],
            None => vec![(true, None), (false, None)],
        };
        for (markdown, keyboard) in attempts {
            let body = message_body(content, msg_id, msg_seq, markdown, keyboard.as_ref());
            let response = self
                .authorized(self.http.post(url))
                .await?
                .json(&body)
                .send()
                .await
                .context("cannot send QQ message")?;
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            // 尝试级日志：定位「服务端只发一次、用户收两条」类双发问题——
            // 若 markdown 尝试返回了错误状态码但平台实际已送达，降级重试会再发一条。
            eprintln!(
                "[qq.send] {} markdown={markdown} keyboard={} status={status} err={}",
                url.rsplit('/').nth(2).unwrap_or(url),
                keyboard.is_some(),
                body.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
            );
            if status.is_success() {
                return Ok(body);
            }
            if !markdown {
                bail!("QQ send message failed with {status}: {body}");
            }
        }
        unreachable!("text fallback must return or fail")
    }

    async fn authorized(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        Ok(request.header(
            "Authorization",
            format!("QQBot {}", self.access_token().await?),
        ))
    }
}

/// 富媒体消息体（msg_type 7）：media.file_info 来自上传接口；
/// 配文走 content，被动回复带 msg_id/msg_seq。
fn media_body(
    caption: &str,
    file_info: &str,
    msg_id: Option<&str>,
    msg_seq: i64,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "msg_type": 7,
        "media": { "file_info": file_info },
    });
    if !caption.is_empty() {
        body["content"] = serde_json::Value::String(caption.to_owned());
    }
    if let Some(msg_id) = msg_id {
        body["msg_id"] = serde_json::Value::String(msg_id.to_owned());
        body["msg_seq"] = serde_json::json!(msg_seq);
    }
    body
}

fn message_body(
    content: &str,
    msg_id: Option<&str>,
    msg_seq: i64,
    markdown: bool,
    keyboard: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut body = if markdown {
        serde_json::json!({ "msg_type": 2, "markdown": { "content": content } })
    } else {
        serde_json::json!({ "msg_type": 0, "content": content })
    };
    if let Some(keyboard) = keyboard {
        body["keyboard"] = keyboard.clone();
    }
    if let Some(msg_id) = msg_id {
        body["msg_id"] = msg_id.into();
        body["msg_seq"] = msg_seq.into();
    }
    body
}

/// 把渠道无关的按钮翻译成 QQ inline keyboard（自定义键盘）。
/// 按钮动作统一为 type 2「指令」：点击 = 以用户身份发送指令文本（enter 自动发送）。
/// 平台上限：5 行 × 每行 4 按钮，超出裁剪。
fn qq_keyboard(markup: &crate::ports::commands::ReplyMarkup) -> Option<serde_json::Value> {
    let rows: Vec<serde_json::Value> = markup
        .rows
        .iter()
        .filter(|row| !row.is_empty())
        .take(5)
        .enumerate()
        .map(|(row_idx, row)| {
            let buttons: Vec<serde_json::Value> = row
                .iter()
                .take(4)
                .enumerate()
                .map(|(col_idx, button)| {
                    serde_json::json!({
                        "id": format!("b{row_idx}_{col_idx}"),
                        "render_data": {
                            "label": button.label,
                            "visited_label": button.label,
                            "style": 0
                        },
                        "action": {
                            "type": 2,
                            "permission": { "type": 0 },
                            "data": button.command,
                            "reply": true,
                            "enter": true
                        }
                    })
                })
                .collect();
            serde_json::json!({ "buttons": buttons })
        })
        .collect();
    if rows.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "content": { "rows": rows } }))
}

pub type SharedQqApi = Arc<QqApiClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_payload_uses_msg_type_seven() {
        let body = media_body("看这张图", "file-info-abc", Some("msg-9"), 3);
        assert_eq!(body["msg_type"], 7);
        assert_eq!(body["media"]["file_info"], "file-info-abc");
        assert_eq!(body["content"], "看这张图");
        assert_eq!(body["msg_id"], "msg-9");
        assert_eq!(body["msg_seq"], 3);
        // 无配文不带 content；主动消息不带 msg_id
        let body = media_body("", "fi", None, 1);
        assert!(body.get("content").is_none());
        assert!(body.get("msg_id").is_none());
    }

    #[test]
    fn markdown_payload_has_no_conflicting_content_field() {
        let body = message_body("**加粗**", Some("msg-1"), 2, true, None);
        assert_eq!(body["msg_type"], 2);
        assert_eq!(body["markdown"]["content"], "**加粗**");
        assert_eq!(body["msg_id"], "msg-1");
        assert_eq!(body["msg_seq"], 2);
        assert!(body.get("content").is_none());
        assert!(body.get("keyboard").is_none());
    }

    #[test]
    fn text_fallback_payload_uses_msg_type_zero() {
        let body = message_body("plain", None, 1, false, None);
        assert_eq!(body["msg_type"], 0);
        assert_eq!(body["content"], "plain");
        assert!(body.get("markdown").is_none());
        assert!(body.get("msg_id").is_none());
    }

    #[test]
    fn keyboard_payload_uses_inline_command_buttons() {
        let markup = crate::ports::commands::ReplyMarkup::new().row(vec![
            crate::ports::commands::MarkupButton::new("任务", "/任务"),
            crate::ports::commands::MarkupButton::new("待办", "/待办"),
        ]);
        let keyboard = qq_keyboard(&markup).unwrap();
        let rows = &keyboard["content"]["rows"];
        assert_eq!(rows[0]["buttons"].as_array().unwrap().len(), 2);
        let first = &rows[0]["buttons"][0];
        assert_eq!(first["action"]["type"], 2);
        assert_eq!(first["action"]["data"], "/任务");
        assert_eq!(first["action"]["enter"], true);
        assert_eq!(first["render_data"]["label"], "任务");
        // 空 markup 不产生键盘。
        assert!(qq_keyboard(&crate::ports::commands::ReplyMarkup::new()).is_none());
    }
}
