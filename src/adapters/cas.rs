//! CAS 算术验证码离线求解器（联奕 kaptchaType=1：`个位数 ±× 个位数 =`）。
//!
//! 管线：PNG 解码 → 亮度二值化（<245，浅色字形可达 236）→ 列投影切割
//! （4 个字形槽位）→ 归一化 12×16 位图 → 模板库最近邻（汉明距离，
//! 超过 REJECT_DISTANCE 判为不确定返回 None，由调用方换新图重试）。
//!
//! 模板库见 captcha_templates.rs，由 tests/fixtures/jwxt_captcha/ 的 40 张
//! 人工标注样本生成，LOO 交叉验证 40/40。校方换验证码风格时：抓新样本、
//! 更新 fixtures 与模板库即可。

use super::cas_templates::GLYPH_TEMPLATES;
use num_bigint::BigUint;
use num_traits::{Num, Zero};

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

/// Ohdave BigInt RSA（联奕 CAS 前端加密，语义经页面 JS 逐字节对照）。
/// 密码按 chunkSize = 2×h(modulus) 字符分块，每块小端打包后 modPow；
/// 输出按 16-bit limb 自高到低每组 4 位 hex，多块以单空格连接。
/// 仅推荐 ASCII 密码（非 ASCII 字符的行为在原 JS 中即未定义）。
pub fn ohdave_encrypt(text: &str, exponent_hex: &str, modulus_hex: &str) -> String {
    let exponent = BigUint::from_str_radix(exponent_hex, 16).expect("invalid RSA exponent");
    let modulus = BigUint::from_str_radix(modulus_hex, 16).expect("invalid RSA modulus");
    // h(modulus)：最高非零 16-bit digit 下标（4 个 hex 字符一组，自高位起）
    let groups: Vec<u16> = modulus_hex
        .as_bytes()
        .chunks(4)
        .map(|chunk| {
            u16::from_str_radix(std::str::from_utf8(chunk).unwrap_or("0"), 16).unwrap_or(0)
        })
        .collect();
    let highest = groups.iter().rposition(|g| *g != 0).unwrap_or(0);
    let chunk = 2 * highest;

    let codes: Vec<u32> = text.chars().map(|c| c as u32).collect();
    let mut blocks = Vec::new();
    for offset in (0..codes.len()).step_by(chunk.max(1)) {
        let slice = &codes[offset..(offset + chunk).min(codes.len())];
        let mut bytes = vec![0u8; chunk];
        for (index, code) in slice.iter().enumerate() {
            bytes[index] = (*code & 0xFF) as u8;
        }
        let m = BigUint::from_bytes_le(&bytes);
        let c = m.modpow(&exponent, &modulus);
        blocks.push(limb_hex(&c));
    }
    blocks.join(" ")
}

/// BigUint → 按 16-bit limb 自高到低的 4 位 hex（每 limb 恒 4 字符，宽度 =
/// (最高非零 limb 下标+1)×4），对齐 Ohdave u(m) 的输出。
fn limb_hex(value: &BigUint) -> String {
    if value.is_zero() {
        return "0000".to_owned();
    }
    let bytes = value.to_bytes_le();
    let mut limbs = Vec::new();
    for chunk in bytes.chunks(2) {
        let low = chunk[0] as u16;
        let high = chunk.get(1).map(|b| (*b as u16) << 8).unwrap_or(0);
        limbs.push(low | high);
    }
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    limbs
        .iter()
        .rev()
        .map(|limb| format!("{limb:04x}"))
        .collect()
}
