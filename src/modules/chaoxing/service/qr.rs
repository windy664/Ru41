//! 二维码照片签到：enc 的唯一来源是二维码图片本身（服务端生成、平台校验
//! 真实性，伪造值被「签到失败，请重新扫描。」拒绝，2026-10-23 实测）；
//! 活动窗口内任意一个历史 enc 均有效（二维码 10 秒轮换不影响旧 enc）。
//! 因此把二维码拍照/截图/转发给机器人即可完成签到：下载图片 → 解码出
//! activeId 与 enc → 以真实 enc 签到 → 回执。

use std::future::Future;
use std::pin::Pin;

use crate::modules::chaoxing::source::{
    ChaoxingActivity, ChaoxingClient, ChaoxingCourse, SignMethod,
};
use crate::ports::commands::ImageConsumer;

use super::enc_sample::{EncSample, EncSampleLogger};
use super::sign::sign_response_ok;

/// 二维码里解出的签到要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSignQr {
    pub activity_id: i64,
    pub enc: String,
}

/// 从图片字节里解学习通签到二维码：仅当图片确实是签到码（chaoxing.com 域、
/// query 同时带 activeId 与 enc）时返回 Some；其余图片返回 None，调用方据此
/// 把消息交还原有通道（agent / 命令）。
pub fn decode_sign_qr(bytes: &[u8]) -> Option<DecodedSignQr> {
    let img = image::load_from_memory(bytes).ok()?.into_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(img);
    for grid in prepared.detect_grids() {
        let Ok((_, text)) = grid.decode() else {
            continue;
        };
        if let Some(qr) = parse_sign_qr_url(&text) {
            return Some(qr);
        }
    }
    None
}

/// 识别签到 URL：host 是 chaoxing.com 系，且 query 同时带活动 id
/// （activeId 或 id）与 enc。其余参数忽略。
pub fn parse_sign_qr_url(text: &str) -> Option<DecodedSignQr> {
    let (path, query) = text.trim().split_once('?')?;
    if !path.contains("chaoxing.com") {
        return None;
    }
    let mut activity_id = None;
    let mut enc = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        match key {
            "activeId" | "id" => activity_id = value.parse().ok(),
            "enc" => enc = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(DecodedSignQr {
        activity_id: activity_id?,
        enc: enc?,
    })
}

/// QQ 图片通道的二维码签到执行器：下载图片 → 解码 → 以真实 enc 签到。
/// 独立 cookie 文件，不与签到轮询进程争写。
pub struct ChaoxingQrSigner {
    client: ChaoxingClient,
    http: reqwest::Client,
    default_location: Option<(f64, f64, String)>,
    /// 科研线 C：每次照片解码出的 enc 都落样本库（成败都记）。
    enc_log: Option<EncSampleLogger>,
}

impl ChaoxingQrSigner {
    pub fn new(
        client: ChaoxingClient,
        default_location: Option<(f64, f64, String)>,
        enc_sample_path: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            client,
            http: reqwest::Client::new(),
            default_location,
            enc_log: enc_sample_path.map(EncSampleLogger::new),
        }
    }

    /// 处理一条图片消息。None = 不是签到码/下载失败（交还原通道）；
    /// Some(回执) = 已按签到码处理（无论成败），消息不再进命令/Agent。
    async fn consume(&self, image_url: &str) -> Option<String> {
        // QQ 附件 URL 不带 scheme（平台怪癖），下载补 https://
        let bytes = self
            .http
            .get(format!("https://{image_url}"))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .bytes()
            .await
            .ok()?;
        let qr = decode_sign_qr(&bytes)?;
        Some(self.sign_with(&qr).await)
    }

    async fn sign_with(&self, qr: &DecodedSignQr) -> String {
        if let Err(err) = self.client.ensure_session().await {
            return format!("❌ 二维码签到失败：登录态不可用（{err:#}）");
        }
        let method = SignMethod::Qr {
            enc: qr.enc.clone(),
            location: self.default_location.clone(),
        };
        let outcome = match self
            .client
            .sign(
                &ChaoxingCourse::bare(),
                &ChaoxingActivity::bare(qr.activity_id),
                &method,
            )
            .await
        {
            Ok(response) => {
                let ok = sign_response_ok(&response);
                (
                    ok,
                    if ok {
                        format!("✅ 二维码签到成功（图片识别，活动 {}）", qr.activity_id)
                    } else {
                        format!(
                            "❌ 二维码签到被平台拒绝（活动 {}，响应：{}）；可能已结束或已签",
                            qr.activity_id,
                            response.trim()
                        )
                    },
                    response,
                )
            }
            Err(err) => (
                false,
                format!("❌ 二维码签到请求失败（活动 {}）：{err:#}", qr.activity_id),
                format!("{err:#}"),
            ),
        };
        // 科研线 C：成败都落样本（拒绝文本是 token 有效期分析的关键数据）
        if let Some(logger) = &self.enc_log {
            logger.log(&EncSample {
                ts: crate::util::now(),
                activity_id: qr.activity_id,
                enc: qr.enc.clone(),
                source: "photo".into(),
                ok: outcome.0,
                response: outcome.2.trim().to_owned(),
            });
        }
        outcome.1
    }
}

impl ImageConsumer for ChaoxingQrSigner {
    fn handle_image<'a>(
        &'a self,
        image_url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(self.consume(image_url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_of(text: &str) -> Vec<u8> {
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let img: image::ImageBuffer<image::Luma<u8>, Vec<u8>> =
            code.render::<image::Luma<u8>>().build();
        let mut png = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    #[test]
    fn roundtrip_sign_qr_png() {
        // 真实形状：pcStuSignController + activeId + enc（附带多余参数）
        let url = "https://mobilelearn.chaoxing.com/widget/sign/pcStuSignController?activeId=1000175604104&enc=1D0A628CK317F44CCC378M5KD92&clientId=x&c=z";
        let qr = decode_sign_qr(&png_of(url)).expect("应解出签到码");
        assert_eq!(qr.activity_id, 1000175604104);
        assert_eq!(qr.enc, "1D0A628CK317F44CCC378M5KD92");
    }

    #[test]
    fn parses_new_style_id_param() {
        let qr =
            parse_sign_qr_url("https://mobilelearn.chaoxing.com/widget/sign/e?id=8812&enc=ABC")
                .unwrap();
        assert_eq!(qr.activity_id, 8812);
        assert_eq!(qr.enc, "ABC");
    }

    #[test]
    fn rejects_non_sign_urls() {
        // 非 chaoxing 域
        assert_eq!(
            parse_sign_qr_url("https://example.com/x?activeId=1&enc=a"),
            None
        );
        // 域对但缺 enc
        assert_eq!(
            parse_sign_qr_url("https://mobilelearn.chaoxing.com/x?activeId=1"),
            None
        );
        // 普通图片（二维码内容无关）不解出
        assert_eq!(decode_sign_qr(&png_of("https://example.com/hello")), None);
    }
}
