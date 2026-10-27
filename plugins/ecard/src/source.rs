use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// 宿舍房间定位参数（来自 ecard 平台，值存本地配置不入库）。
#[derive(Debug, Clone, Default)]
pub struct EcardRoom {
    pub impl_type: String,
    pub school_area_no: String,
    pub building_no: String,
    pub room_num: String,
}

/// 一次水电余额读数。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EcardBalance {
    #[serde(rename = "powerBalance", default)]
    pub power: Option<String>,
    #[serde(rename = "waterBalance", default)]
    pub water: Option<String>,
    #[serde(rename = "formatPowerBalanceStr", default)]
    pub power_label: Option<String>,
    #[serde(rename = "formatWaterBalanceStr", default)]
    pub water_label: Option<String>,
    #[serde(rename = "schoolArea", default)]
    pub school_area: Option<String>,
    #[serde(rename = "building", default)]
    pub building: Option<String>,
    #[serde(rename = "room", default)]
    pub room: Option<String>,
}

impl EcardBalance {
    pub fn power_value(&self) -> Option<f64> {
        self.power.as_deref()?.parse().ok()
    }
    pub fn water_value(&self) -> Option<f64> {
        self.water.as_deref()?.parse().ok()
    }
}

#[derive(Debug, Deserialize)]
struct EcardResponse {
    ret: bool,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    obj: Option<EcardBalance>,
}

/// G 校校园卡 ecard 平台客户端。水电查询不校验签名，凭 token 即可；
/// 平台有 WAF 限流，调用方控制节奏（每日个位数请求），失败由上层退避。
/// base_url 走本地配置（[sources.ecard] base_url），学校域名不写入代码。
#[derive(Clone)]
pub struct EcardClient {
    http: reqwest::Client,
    base_url: String,
}

impl EcardClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_base_url(base_url)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            http: ru41::util::http_client(),
            base_url: base_url.into(),
        }
    }

    /// 查询房间水电余额。
    pub async fn get_balance(&self, room: &EcardRoom, token: &str) -> Result<EcardBalance> {
        let response: EcardResponse = self
            .http
            .post(format!("{}/powerfee/getBalance", self.base_url))
            .form(&[
                ("implType", room.impl_type.as_str()),
                ("schoolAreaNo", room.school_area_no.as_str()),
                ("buildingNo", room.building_no.as_str()),
                ("roomNum", room.room_num.as_str()),
                ("from", "wxminiprogram"),
                ("token", token),
            ])
            .send()
            .await
            .context("ecard getBalance request failed")?
            .json()
            .await
            .context("ecard getBalance returned invalid json")?;
        if !response.ret {
            // token 过期也走这里，上层据此提示重新抓包。
            bail!(
                "ecard getBalance rejected: {}",
                response.msg.as_deref().unwrap_or("unknown")
            );
        }
        response.obj.context("ecard getBalance missing obj")
    }
}

impl Default for EcardClient {
    /// 仅供测试或临时使用；正式路径必须显式传入 base_url。
    fn default() -> Self {
        Self::with_base_url("http://localhost")
    }
}
