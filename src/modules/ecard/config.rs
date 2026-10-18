use serde::Deserialize;

/// 校园卡水电监控配置；ecard poll/run 使用。协议见 docs/ecard-waterfee.md。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EcardSection {
    /// ecard 平台 API 基址（如 https://ecarduser.<school>.edu.cn），学校域名只存本地。
    pub base_url: Option<String>,
    /// ecard 平台 token（建议用环境变量 RU41_ECARD_TOKEN 注入，勿提交）。
    pub token: Option<String>,
    /// 房间定位参数（getBalance 四项）。
    pub impl_type: Option<String>,
    pub school_area_no: Option<String>,
    pub building_no: Option<String>,
    pub room_num: Option<String>,
    /// 电量低余额阈值（度），默认 10。
    pub power_threshold: Option<f64>,
    /// 水量低余额阈值（吨），默认 1。
    pub water_threshold: Option<f64>,
    /// ecard run 的轮询间隔（秒），默认 86400（每日）。
    pub interval_secs: Option<u64>,
    /// 低余额时是否经默认通知出口提醒；默认 true。
    pub notify: Option<bool>,
}
