//! CAS 算术验证码离线求解器（联奕 kaptchaType=1：`个位数 ±× 个位数 =`）。
//!
//! 管线：PNG 解码 → 亮度二值化（<245，浅色字形可达 236）→ 列投影切割
//! （4 个字形槽位）→ 归一化 12×16 位图 → 模板库最近邻（汉明距离，
//! 超过 REJECT_DISTANCE 判为不确定返回 None，由调用方换新图重试）。
//!
//! 模板库见 captcha_templates.rs，由 tests/fixtures/jwxt_captcha/ 的 40 张
//! 人工标注样本生成，LOO 交叉验证 40/40。校方换验证码风格时：抓新样本、
//! 更新 fixtures 与模板库即可。

use super::captcha_templates::GLYPH_TEMPLATES;

/// 归一化网格尺寸。
pub const GRID_W: usize = 12;
pub const GRID_H: usize = 16;

/// 二值化亮度阈值：背景纯白，最浅的字形亮度约 236。
const INK_THRESHOLD: u8 = 245;
/// 列投影切割时，间隔不超过该值的碎片视为同一字形。
const GLYPH_GAP: u32 = 2;
/// 超过该汉明距离判为“不认识”（LOO 实测正确匹配 ≤31/192）。
const REJECT_DISTANCE: u32 = 48;

/// 求解算术验证码 PNG，返回答案字符串（如 "56"）；认不出返回 None。
pub fn solve(png_bytes: &[u8]) -> Option<String> {
    let (mask, w, h) = decode_and_binarize(png_bytes)?;
    let boxes = segment(&mask, w, h);
    if boxes.len() != 4 {
        return None;
    }
    let glyphs: Vec<Glyph> = boxes.iter().map(|b| normalize(&mask, w, h, *b)).collect();
    let mut chars = String::new();
    for glyph in &glyphs {
        chars.push(classify(glyph)?);
    }
    evaluate(&chars)
}

/// 位图字形（12×16=192 bit，u64×3 小端位序，与模板库一致）。
pub type Glyph = [u64; 3];

fn decode_and_binarize(png_bytes: &[u8]) -> Option<(Vec<bool>, usize, usize)> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let bytes = &buf[..info.buffer_size()];
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        _ => return None,
    };
    let mut mask = vec![false; w * h];
    for (i, px) in bytes.chunks(channels).take(w * h).enumerate() {
        let lum = match channels {
            1 => px[0] as u32,
            _ => (px[0] as u32 + px[1] as u32 + px[2] as u32) / 3,
        };
        mask[i] = lum < INK_THRESHOLD as u32;
    }
    Some((mask, w, h))
}

fn segment(mask: &[bool], w: usize, h: usize) -> Vec<(usize, usize)> {
    let col_ink = |x: usize| (0..h).any(|y| mask[y * w + x]);
    let mut runs = Vec::new();
    let mut start = None;
    for x in 0..w {
        if col_ink(x) && start.is_none() {
            start = Some(x);
        } else if !col_ink(x)
            && let Some(s) = start
        {
            runs.push((s, x - 1));
            start = None;
        }
    }
    if let Some(s) = start {
        runs.push((s, w - 1));
    }
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in runs {
        if let Some(last) = merged.last_mut()
            && r.0 as u32 - last.1 as u32 <= GLYPH_GAP + 1
        {
            last.1 = r.1;
            continue;
        }
        merged.push(r);
    }
    merged
}

fn normalize(mask: &[bool], w: usize, h: usize, box_: (usize, usize)) -> Glyph {
    let (x0, x1) = box_;
    let mut y0 = h;
    let mut y1 = 0;
    for y in 0..h {
        for x in x0..=x1 {
            if mask[y * w + x] {
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    let (gh, gw) = (y1 - y0 + 1, x1 - x0 + 1);
    let mut glyph: Glyph = [0; 3];
    for gy in 0..GRID_H {
        for gx in 0..GRID_W {
            let sy = y0 + gy * gh / GRID_H;
            let sx = x0 + gx * gw / GRID_W;
            if mask[sy * w + sx] {
                let bit = gy * GRID_W + gx;
                glyph[bit / 64] |= 1u64 << (bit % 64);
            }
        }
    }
    glyph
}

fn classify(glyph: &Glyph) -> Option<char> {
    let (ch, distance) = GLYPH_TEMPLATES
        .iter()
        .map(|(ch, tpl)| (*ch, hamming(glyph, tpl)))
        .min_by_key(|(_, d)| *d)?;
    (distance <= REJECT_DISTANCE).then_some(ch)
}

fn hamming(a: &Glyph, b: &Glyph) -> u32 {
    a.iter().zip(b).map(|(x, y)| (x ^ y).count_ones()).sum()
}

/// "8+2=" / "9*2=" → 计算结果。
fn evaluate(chars: &str) -> Option<String> {
    let chars = chars.strip_suffix('=')?;
    let (a, rest) = chars.split_at(1);
    let (op, b) = rest.split_at(1);
    let a: u32 = a.parse().ok()?;
    let b: u32 = b.parse().ok()?;
    let result = match op {
        "+" => a + b,
        "*" | "×" => a * b,
        "-" => a.checked_sub(b)?,
        _ => return None,
    };
    Some(result.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_all_labeled_fixtures() {
        let labels: std::collections::BTreeMap<String, String> = serde_json::from_str(
            &std::fs::read_to_string("tests/fixtures/jwxt_captcha/labels.json").unwrap(),
        )
        .unwrap();
        assert_eq!(labels.len(), 40);
        for (name, expr) in &labels {
            let png = std::fs::read(format!("tests/fixtures/jwxt_captcha/{name}.png")).unwrap();
            let expected = evaluate(&format!("{expr}=")).unwrap();
            let got = solve(&png).unwrap_or_else(|| panic!("fixture {name} ({expr}) 未识别"));
            assert_eq!(got, expected, "fixture {name} ({expr}) 解错");
        }
    }

    #[test]
    fn garbage_returns_none_not_panic() {
        assert_eq!(solve(b"not a png"), None);
        assert_eq!(solve(&[]), None);
    }
}
