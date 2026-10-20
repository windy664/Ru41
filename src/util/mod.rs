//! 少量跨层通用的小工具。保持克制：只在出现真实重复时才把代码收进这里。

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
