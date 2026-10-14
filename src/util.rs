//! 少量跨层通用的小工具。保持克制：只在出现真实重复时才把代码收进这里。

/// 错误信息入库前的最大字符数，防止超长输出撑爆记录。
pub const ERROR_MAX_CHARS: usize = 2000;

/// 按字符数截断（不按字节，避免截断 UTF-8 序列）。
pub fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
