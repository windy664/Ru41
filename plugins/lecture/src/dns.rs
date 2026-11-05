//! QQ CDN 下载的 DNS 兜底。
//!
//! 家宽/校园网的系统 DNS 解析 `grouptalk.c2c.qq.com` 时常返回被污染的边缘 IP
//! —— 能解析、连不上，表现是 connect timeout；浏览器能下是因为它走了 DoH。
//! 直连失败后用 DoH 解析出的 IP 重试，避免「链接还有效却下不下来」。

use std::net::IpAddr;
use std::time::Duration;

/// DoH 端点（阿里 / 腾讯），都返回 DNS-JSON。
const DOH_ENDPOINTS: &[&str] = &[
    "https://223.5.5.5/resolve?name={name}&type=A",
    "https://1.12.12.12/dns-query?name={name}&type=A",
];

/// DoH 查询用的小客户端：短超时，一个端点不通就换下一个。
fn doh_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(8))
        .build()
        .expect("doh client")
}

/// 用 DoH 解析 A 记录，返回候选 IP（按端点给出的顺序）。
pub async fn resolve_a(host: &str) -> Vec<IpAddr> {
    let client = doh_client();
    let mut out: Vec<IpAddr> = Vec::new();
    for template in DOH_ENDPOINTS {
        let url = template.replace("{name}", host);
        let Ok(response) = client
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
        else {
            continue;
        };
        let Ok(body) = response.json::<serde_json::Value>().await else {
            continue;
        };
        let Some(answers) = body.get("Answer").and_then(|a| a.as_array()) else {
            continue;
        };
        for answer in answers {
            let Some(data) = answer.get("data").and_then(|d| d.as_str()) else {
                continue;
            };
            let Ok(ip) = data.parse::<IpAddr>() else {
                continue;
            };
            if ip.is_ipv4() && !out.contains(&ip) {
                out.push(ip);
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_cover_both_providers() {
        assert!(DOH_ENDPOINTS.iter().any(|e| e.contains("223.5.5.5")));
        assert!(DOH_ENDPOINTS.iter().any(|e| e.contains("1.12.12.12")));
    }
}
