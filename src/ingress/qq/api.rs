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
            http: reqwest::Client::new(),
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
            msg_id,
            msg_seq,
        )
        .await
    }

    async fn post_message(
        &self,
        url: &str,
        content: &str,
        msg_id: Option<&str>,
        msg_seq: i64,
    ) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({ "content": content, "msg_type": 0 });
        if let Some(msg_id) = msg_id {
            body["msg_id"] = msg_id.into();
            body["msg_seq"] = msg_seq.into();
        }
        let response = self
            .authorized(self.http.post(url))
            .await?
            .json(&body)
            .send()
            .await
            .context("cannot send QQ message")?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("QQ send message failed with {status}: {body}");
        }
        Ok(body)
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

pub type SharedQqApi = Arc<QqApiClient>;
