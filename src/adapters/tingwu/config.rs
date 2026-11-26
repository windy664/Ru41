//! 听悟客户端的运行参数（宿主侧）。
//!
//! 插件（`plugins/tingwu`）从自己的 `[sources.tingwu]` 配置段解析出这些值，
//! 宿主与插件共用同一份客户端 —— 所以参数结构放在宿主 crate。

use std::time::Duration;

/// 听悟转写客户端的运行参数。
#[derive(Debug, Clone)]
pub struct TingwuConfig {
    /// 轮询转写结果的超时（秒）。
    pub poll_timeout: Duration,
    /// 轮询间隔（秒）。
    pub poll_interval: Duration,
    /// 单文件大小上限（字节）。
    pub max_file_bytes: u64,
    /// 同一人两句话间隔小于这个毫秒数就并进同一段。
    pub merge_gap_ms: i64,
    /// 本地 ASR 的基址（容器间服务名）。
    pub local_asr_base_url: String,
    /// 本地 ASR 的接口形态：`whispercpp` / `openai`。
    pub local_asr_api: String,
}

impl Default for TingwuConfig {
    fn default() -> Self {
        Self {
            poll_timeout: Duration::from_secs(900),
            poll_interval: Duration::from_secs(5),
            max_file_bytes: 500 * 1024 * 1024,
            merge_gap_ms: 5_000,
            local_asr_base_url: "http://paraformer-asr:50000".to_owned(),
            local_asr_api: "whispercpp".to_owned(),
        }
    }
}
