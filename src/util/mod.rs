//! 少量跨层通用的小工具。保持克制：只在出现真实重复时才把代码收进这里。

use std::net::ToSocketAddrs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 出站 HTTP 请求的整体超时；抓取/通知类调用都不应无限等待。
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 出站 HTTP 客户端统一出口：带整体超时与 ru41 UA，避免各来源重复构造、漏设超时。
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("ru41/", env!("CARGO_PKG_VERSION")))
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("reqwest client build")
}

/// 当前 Unix 秒；全库统一用它，避免各处时间源不一致。
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 错误信息入库前的最大字符数，防止超长输出撑爆记录。
pub const ERROR_MAX_CHARS: usize = 2000;

/// 按字符数截断（不按字节，避免截断 UTF-8 序列）。
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// 当前 UTC 日期（YYYY-MM-DD）；用于 digest 幂等键这类按日分组的场景。
pub fn today_utc(now: i64) -> String {
    let days = now.div_euclid(86_400);
    // 简化的民用日期换算（1970-01-01 起）。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// DNS 兜底解析的结果：需要绕过系统解析、直连该地址的 host。
pub struct DnsOverride {
    pub host: String,
    pub addr: std::net::SocketAddr,
}

/// 对每个 (host, port)：系统解析成功则不管；失败则查公共 DoH（doh.pub JSON API），
/// 拿到 A 记录就生成 [`DnsOverride`]（reqwest `ClientBuilder::resolve` 注入），
/// 查不到/超时则跳过——由正常解析路径报错，行为退回原样。
///
/// 背景：部分高校自管权威 DNS 半残（NS 服务器自身无公网 A 记录），逐级递归会
/// SERVFAIL，而公共解析器的缓存里仍有有效记录。只在构建客户端时调用一次；
/// 若学校中途换 IP（极罕见），下个进程重启自然纠正。
pub fn dns_fallback_overrides(hosts: &[(&str, u16)]) -> Vec<DnsOverride> {
    hosts
        .iter()
        .filter(|(host, port)| system_lookup(host, *port).is_none())
        .filter_map(|(host, port)| {
            let ip = doh_lookup(host)?;
            Some(DnsOverride {
                host: (*host).to_owned(),
                addr: std::net::SocketAddr::new(ip, *port),
            })
        })
        .collect()
}

fn system_lookup(host: &str, port: u16) -> Option<std::net::SocketAddr> {
    (host, port).to_socket_addrs().ok()?.next()
}

/// 查询 doh.pub 的 JSON API 取第一条 A 记录。doh.pub 在境内可直连，
/// 查询本身走 std 线程（reqwest::blocking 不能进 tokio 运行时）。
fn doh_lookup(host: &str) -> Option<std::net::IpAddr> {
    use std::sync::mpsc;
    let host = host.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let url = format!("{DOH_ENDPOINT}?name={host}&type=A");
        let result = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(DOH_TIMEOUT_SECS))
            .build()
            .ok()
            .and_then(|client| client.get(&url).send().ok())
            .and_then(|resp| resp.json::<serde_json::Value>().ok())
            .and_then(|json| {
                (json["Status"].as_i64() == Some(0))
                    .then_some(json)
                    .and_then(|json| {
                        json["Answer"]
                            .as_array()?
                            .iter()
                            .filter(|a| a["type"].as_i64() == Some(1))
                            .find_map(|a| a["data"].as_str()?.parse().ok())
                    })
            });
        let _ = tx.send(result);
    });
    rx.recv_timeout(Duration::from_secs(DOH_TIMEOUT_SECS * 2))
        .ok()?
}

const DOH_ENDPOINT: &str = "https://doh.pub/dns-query";
const DOH_TIMEOUT_SECS: u64 = 3;
