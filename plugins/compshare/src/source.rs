use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::service::CompshareConfig;

/// 一条可创建规格；字段名与 CheckCompShareResourceCapacity 响应一致。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacitySpec {
    #[serde(rename = "Cpu")]
    pub cpu: u64,
    #[serde(rename = "Gpu")]
    pub gpu: u64,
    #[serde(rename = "Mem")]
    pub mem: u64,
    #[serde(rename = "ResourceEnough")]
    pub resource_enough: bool,
}

/// 库存业务状态；只有 capacity 接口明确返回 ResourceEnough 才算有货。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StockState {
    Available,
    Unavailable,
    AuthExpired,
}

/// 一次容量查询结果。
#[derive(Debug, Clone, Serialize)]
pub struct StockSnapshot {
    pub state: StockState,
    pub specs: Vec<CapacitySpec>,
}

#[derive(Debug, Deserialize)]
struct CapacityResponse {
    #[serde(rename = "RetCode")]
    ret_code: i64,
    #[serde(rename = "Message", default)]
    message: Option<String>,
    #[serde(rename = "Specs", default)]
    specs: Vec<CapacitySpec>,
}

#[derive(Clone)]
pub struct CompshareClient {
    http: reqwest::Client,
    base_url: String,
}

impl CompshareClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: ru41::util::http_client(),
            base_url: base_url.into(),
        }
    }

    /// 查询当前镜像 + region/zone/GPU 的可创建容量。
    pub async fn check_capacity(&self, config: &CompshareConfig) -> Result<StockSnapshot> {
        let csrf = cookie_value(&config.cookie, "U_CSRF_TOKEN")
            .context("compshare cookie missing U_CSRF_TOKEN")?;
        let encoded_user = cookie_value(&config.cookie, "U_USER_EMAIL")
            .context("compshare cookie missing U_USER_EMAIL")?;
        let user = decode_form_value(encoded_user).context("invalid U_USER_EMAIL cookie")?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system time before unix epoch")?
            .as_millis()
            .to_string();
        let disk_size = config.disk_size_gb.to_string();
        let params = vec![
            ("ProjectId", config.project_id.as_str()),
            ("ChargeType", "Postpay"),
            ("CompShareImageId", config.image_id.as_str()),
            ("Region", config.region.as_str()),
            ("Zone", config.zone.as_str()),
            ("GPUType", config.gpu_type.as_str()),
            ("Disks[0][IsBoot]", "true"),
            ("Disks[0][Type]", "CLOUD_SSD"),
            ("Disks[0][Size]", disk_size.as_str()),
            ("MachineType", "G"),
            ("MinimalCpuPlatform", "Auto"),
            ("Action", "CheckCompShareResourceCapacity"),
            ("_user", user.as_str()),
            ("_timestamp", timestamp.as_str()),
        ];
        let response = self
            .http
            .post(&self.base_url)
            .header(reqwest::header::COOKIE, &config.cookie)
            .header("U-CSRF-Token", csrf)
            .header(reqwest::header::REFERER, config.page_url())
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .form(&params)
            .send()
            .await
            .context("compshare capacity request failed")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("compshare capacity response read failed")?;
        if status == reqwest::StatusCode::UNAUTHORIZED
            || status == reqwest::StatusCode::FORBIDDEN
            || is_expired_message(&text)
        {
            return Ok(StockSnapshot {
                state: StockState::AuthExpired,
                specs: Vec::new(),
            });
        }
        if !status.is_success() {
            bail!("compshare capacity returned HTTP {status}");
        }
        let parsed: CapacityResponse = serde_json::from_str(&text)
            .with_context(|| format!("compshare capacity invalid json: {}", truncate(&text)))?;
        if parsed.ret_code != 0 {
            let message = parsed.message.unwrap_or_else(|| "unknown".into());
            if is_expired_message(&message) {
                return Ok(StockSnapshot {
                    state: StockState::AuthExpired,
                    specs: Vec::new(),
                });
            }
            bail!(
                "compshare capacity rejected: RetCode {}: {}",
                parsed.ret_code,
                truncate(&message)
            );
        }
        let available = parsed.specs.iter().any(|spec| spec.resource_enough);
        Ok(StockSnapshot {
            state: if available {
                StockState::Available
            } else {
                StockState::Unavailable
            },
            specs: parsed.specs,
        })
    }
}

fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

fn decode_form_value(value: &str) -> Result<String> {
    let input = format!("value={value}");
    url::form_urlencoded::parse(input.as_bytes())
        .find(|(key, _)| key == "value")
        .map(|(_, value)| value.into_owned())
        .context("missing form value")
}

fn is_expired_message(text: &str) -> bool {
    [
        "Missing signature",
        "missing signature",
        "未登录",
        "登录",
        "失效",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn truncate(text: &str) -> String {
    ru41::util::truncate_chars(text, 300)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_and_form_decoding() {
        let cookie = "U_USER_EMAIL=user%40example.edu.cn; U_CSRF_TOKEN=csrf; U_JWT_TOKEN=jwt";
        assert_eq!(cookie_value(cookie, "U_CSRF_TOKEN"), Some("csrf"));
        assert_eq!(
            decode_form_value(cookie_value(cookie, "U_USER_EMAIL").unwrap()).unwrap(),
            "user@example.edu.cn"
        );
    }

    #[test]
    fn expired_markers() {
        assert!(is_expired_message(
            r#"{"RetCode":170,"Message":"Missing signature"}"#
        ));
        assert!(!is_expired_message("正常"));
    }
}
