//! 学习通滑块验证码（90002 风控破局）的纯函数层：参数构造、JSONP 解析、缺口识别。
//! HTTP 流程在 client.rs 的 solve_slider_captcha；方案移植自社区 ChaoxingSignFaker
//! 的纯 HTTP 实现（getCaptchaImageV2 / checkCaptchaResult），缺口识别用
//! 拼块边缘 + Sobel 模板匹配，不依赖 ML 模型。

use anyhow::{Context, Result, bail};
use md5::{Digest as _, Md5};

/// 学习通 web 端滑块的固定 captchaId（社区长期在用）。
pub const CAPTCHA_ID: &str = "Qt9FIw9o4pwRjOyqM6yizZBh682qN2TU";
pub const CAPTCHA_VERSION: &str = "1.1.20";
const JSONP_WRAP: &str = "cx_captcha_function";

fn md5_hex(input: &str) -> String {
    Md5::digest(input.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn rand_hex32() -> String {
    format!("{:032x}", rand::random::<u128>())
}

/// image 请求的本地参数三元组 (captchaKey, iv, token)：
/// captchaKey = md5(t + 随机串)；iv = md5(captchaId + "slide" + now + 随机串)；
/// token = md5(t + captchaId + "slide" + captchaKey) + ":" + (t + 300000)。
pub fn image_params(t: i64, now_ms: i64) -> (String, String, String) {
    let captcha_key = md5_hex(&format!("{t}{}", rand_hex32()));
    let iv = md5_hex(&format!("{CAPTCHA_ID}slide{now_ms}{}", rand_hex32()));
    let token = format!(
        "{}:{}",
        md5_hex(&format!("{t}{CAPTCHA_ID}slide{captcha_key}")),
        t + 300_000
    );
    (captcha_key, iv, token)
}

/// 去掉 JSONP 包装 `cx_captcha_function(...)`。
pub fn unwrap_jsonp(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix(JSONP_WRAP)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(text)
        .trim()
}

/// conf 响应取服务端时间戳 t。
pub fn parse_conf_t(text: &str) -> Result<i64> {
    let value: serde_json::Value =
        serde_json::from_str(unwrap_jsonp(text)).context("captcha conf 响应不是合法 JSONP")?;
    value
        .get("t")
        .and_then(serde_json::Value::as_i64)
        .context("captcha conf 响应缺少 t")
}

/// image 响应取 (服务端 token, 底图 URL, 拼块图 URL)。
/// 注意此 token 是服务端下发的，与本地构造的 image-token 不是一个东西。
pub fn parse_image_response(text: &str) -> Result<(String, String, String)> {
    let value: serde_json::Value =
        serde_json::from_str(unwrap_jsonp(text)).context("captcha image 响应不是合法 JSONP")?;
    let token = value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .context("captcha image 响应缺少 token")?
        .to_owned();
    let vo = value
        .get("imageVerificationVo")
        .context("captcha image 响应缺少 imageVerificationVo")?;
    let shade = vo
        .get("shadeImage")
        .and_then(serde_json::Value::as_str)
        .context("缺少 shadeImage")?
        .to_owned();
    let cutout = vo
        .get("cutoutImage")
        .and_then(serde_json::Value::as_str)
        .context("缺少 cutoutImage")?
        .to_owned();
    Ok((token, shade, cutout))
}

/// check 响应：error==1 是接口级报错；result=true 时 extraData 里有 validate；
/// result=false 是位置不对（可换图重试）。
pub fn parse_check_validate(text: &str) -> Result<Option<String>> {
    let value: serde_json::Value =
        serde_json::from_str(unwrap_jsonp(text)).context("captcha check 响应不是合法 JSONP")?;
    if value.get("error").and_then(serde_json::Value::as_i64) == Some(1) {
        bail!(
            "captcha check 报错: {}",
            value
                .get("msg")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("未知")
        );
    }
    if value.get("result").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let extra = value
        .get("extraData")
        .and_then(serde_json::Value::as_str)
        .context("captcha check 缺少 extraData")?;
    let extra: serde_json::Value =
        serde_json::from_str(&extra.replace("\\\"", "\"")).context("extraData 不是合法 JSON")?;
    let validate = extra
        .get("validate")
        .and_then(serde_json::Value::as_str)
        .context("extraData 缺少 validate")?;
    Ok(Some(validate.to_owned()))
}

/// 滑块缺口 x 识别：拼块（alpha 边界，无 alpha 退化矩形边框）在底图 Sobel 边缘图上
/// 全图滑动模板匹配，返回最佳匹配位置的缺口左缘 x（底图像素坐标）。
/// 拼块图可能是整幅透明画布（拼块在目标 y 处），也可能裁到拼块本身——
/// 用边界点相对画布的偏移加回，两种形态都兼容。
pub fn slider_gap_x(shade_bytes: &[u8], cutout_bytes: &[u8]) -> Option<u32> {
    let shade = image::load_from_memory(shade_bytes).ok()?.to_luma8();
    let cutout = image::load_from_memory(cutout_bytes).ok()?.to_rgba8();
    let (sw, sh) = shade.dimensions();
    let (cw, ch) = cutout.dimensions();
    if cw == 0 || ch == 0 || cw > sw || ch > sh {
        return None;
    }

    let has_alpha = cutout.pixels().any(|p| p.0[3] < 250);
    let opaque = |x: u32, y: u32| !has_alpha || cutout.get_pixel(x, y).0[3] >= 250;
    let mut boundary = Vec::new();
    let mut min_x = cw;
    for y in 0..ch {
        for x in 0..cw {
            if !opaque(x, y) {
                continue;
            }
            let is_edge = x == 0
                || y == 0
                || x == cw - 1
                || y == ch - 1
                || !opaque(x - 1, y)
                || !opaque(x + 1, y)
                || !opaque(x, y - 1)
                || !opaque(x, y + 1);
            if is_edge {
                boundary.push((x, y));
                min_x = min_x.min(x);
            }
        }
    }
    if boundary.is_empty() || min_x == cw {
        return None;
    }

    // 全图 Sobel 边缘强度（算一次，滑动时直接查表）
    let mut sobel = vec![0u32; (sw * sh) as usize];
    let lum = |x: i64, y: i64| -> i64 {
        if x < 0 || y < 0 || x >= sw as i64 || y >= sh as i64 {
            0
        } else {
            shade.get_pixel(x as u32, y as u32).0[0] as i64
        }
    };
    for y in 0..sh as i64 {
        for x in 0..sw as i64 {
            let gx = -lum(x - 1, y - 1) - 2 * lum(x - 1, y) - lum(x - 1, y + 1)
                + lum(x + 1, y - 1)
                + 2 * lum(x + 1, y)
                + lum(x + 1, y + 1);
            let gy = -lum(x - 1, y - 1) - 2 * lum(x, y - 1) - lum(x + 1, y - 1)
                + lum(x - 1, y + 1)
                + 2 * lum(x, y + 1)
                + lum(x + 1, y + 1);
            sobel[(y as u32 * sw + x as u32) as usize] = (gx.abs() + gy.abs()) as u32;
        }
    }

    let mut best: Option<(u64, u32)> = None;
    for oy in 0..=(sh - ch) {
        for ox in 0..=(sw - cw) {
            let score: u64 = boundary
                .iter()
                .map(|&(bx, by)| sobel[((oy + by) * sw + ox + bx) as usize] as u64)
                .sum();
            if best.is_none_or(|(s, _)| score > s) {
                best = Some((score, ox));
            }
        }
    }
    best.map(|(_, x)| x + min_x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_params_follow_community_construction() {
        let (key, iv, token) = image_params(1000, 2000);
        assert_eq!(key.len(), 32);
        assert_eq!(iv.len(), 32);
        let (digest, expiry) = token.split_once(':').unwrap();
        assert_eq!(digest.len(), 32);
        assert_eq!(expiry, "301000");
        // token 与 captchaKey 的绑定关系必须与官方一致
        assert_eq!(digest, md5_hex(&format!("1000{CAPTCHA_ID}slide{key}")));
    }

    #[test]
    fn jsonp_unwraps_and_parses() {
        let wrapped = r#"cx_captcha_function({"t":1793350000,"code":0})"#;
        assert_eq!(parse_conf_t(wrapped).unwrap(), 1793350000);
        // 无包装也容忍
        assert_eq!(parse_conf_t(r#"{"t":7}"#).unwrap(), 7);

        let image = r#"cx_captcha_function({"token":"srv-token","imageVerificationVo":{"shadeImage":"https://x/shade.png","cutoutImage":"https://x/cut.png"}})"#;
        let (token, shade, cutout) = parse_image_response(image).unwrap();
        assert_eq!(token, "srv-token");
        assert_eq!(shade, "https://x/shade.png");
        assert_eq!(cutout, "https://x/cut.png");

        let ok = r#"cx_captcha_function({"error":0,"result":true,"extraData":"{\"validate\":\"v-token-1\"}"})"#;
        assert_eq!(
            parse_check_validate(ok).unwrap().as_deref(),
            Some("v-token-1")
        );
        let miss = r#"cx_captcha_function({"error":0,"result":false})"#;
        assert_eq!(parse_check_validate(miss).unwrap(), None);
        let err = r#"cx_captcha_function({"error":1,"msg":"token 过期"})"#;
        assert!(
            parse_check_validate(err)
                .unwrap_err()
                .to_string()
                .contains("token 过期")
        );
    }

    /// 合成滑块场景：底图在 (120, 30) 处有 40x40 缺口（暗化+亮边），
    /// 拼块图是整幅透明画布、拼块画在缺口同 y 处——识别应给出缺口左缘 120。
    #[test]
    fn slider_gap_detects_notch_in_full_canvas_cutout() {
        use image::{ImageBuffer, Luma, Rgba, RgbaImage};
        let (w, h, size, gap_x, gap_y) = (200u32, 100u32, 40u32, 120u32, 30u32);

        let mut shade: ImageBuffer<Luma<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Luma([200]));
        for y in gap_y..gap_y + size {
            for x in gap_x..gap_x + size {
                let border =
                    x == gap_x || y == gap_y || x == gap_x + size - 1 || y == gap_y + size - 1;
                shade.put_pixel(x, y, Luma([if border { 255 } else { 60 }]));
            }
        }
        let mut cutout: RgbaImage = ImageBuffer::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        for y in gap_y..gap_y + size {
            for x in gap_x..gap_x + size {
                cutout.put_pixel(x, y, Rgba([120, 120, 120, 255]));
            }
        }

        let mut shade_png = Vec::new();
        image::DynamicImage::ImageLuma8(shade)
            .write_to(
                &mut std::io::Cursor::new(&mut shade_png),
                image::ImageFormat::Png,
            )
            .unwrap();
        let mut cutout_png = Vec::new();
        image::DynamicImage::ImageRgba8(cutout)
            .write_to(
                &mut std::io::Cursor::new(&mut cutout_png),
                image::ImageFormat::Png,
            )
            .unwrap();

        assert_eq!(slider_gap_x(&shade_png, &cutout_png), Some(gap_x));
    }

    /// 拼块图裁到拼块本身（无 alpha 的 JPEG 形态）：边界退化为矩形边框，同样命中。
    #[test]
    fn slider_gap_detects_notch_with_cropped_cutout() {
        use image::{ImageBuffer, Luma, Rgb, RgbImage};
        let (w, h, size, gap_x, gap_y) = (200u32, 100u32, 40u32, 60u32, 20u32);

        let mut shade: ImageBuffer<Luma<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Luma([200]));
        for y in gap_y..gap_y + size {
            for x in gap_x..gap_x + size {
                let border =
                    x == gap_x || y == gap_y || x == gap_x + size - 1 || y == gap_y + size - 1;
                shade.put_pixel(x, y, Luma([if border { 255 } else { 60 }]));
            }
        }
        let piece: RgbImage = ImageBuffer::from_pixel(size, size, Rgb([120, 120, 120]));

        let mut shade_png = Vec::new();
        image::DynamicImage::ImageLuma8(shade)
            .write_to(
                &mut std::io::Cursor::new(&mut shade_png),
                image::ImageFormat::Png,
            )
            .unwrap();
        let mut piece_jpg = Vec::new();
        image::DynamicImage::ImageRgb8(piece)
            .write_to(
                &mut std::io::Cursor::new(&mut piece_jpg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        assert_eq!(slider_gap_x(&shade_png, &piece_jpg), Some(gap_x));
    }
}
