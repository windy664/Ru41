//! QQ 专有排版增强：**唯一一处 `qqbot cmd input` 适配**。
//!
//! QQ 侧两个平台能力差异，统一在这里落地，别处不再出现 QQ 专有标签：
//!
//! 1. **行内可点复选框**：规范层的 `{{cmd:符号|指令}}` 占位符 →
//!    `<qqbot-cmd-input text="…" show="…" reference="false" />`
//!    （点一下把指令填进输入框，再点发送即执行——QQ 官方能力的两跳代价）。
//! 2. **字号**：QQ 的 markdown 没有字号语法，只能靠标题层级放大。
//!    栏目标题 → 一级标题 + 加粗；条目行 → 三级标题。
//!
//! 其它渠道走通用降级（`channels::markdown`）：占位符 → 纯文本符号。

use ru41_plugin_api::interactive::{cmd_tokens, has_cmd_token};

/// 把中性排版渲染成 QQ 版式。
pub fn qq_render(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let is_item = has_cmd_token(line);
        let expanded = expand_cmd_tokens(line);
        if is_item {
            // 条目行：三级标题放大（层级：## 栏目标题 > ### 正文条目），
            // 三级标题取代列表符——QQ 的条目原本就不是列表，是标题行。
            out.push_str("### ");
            out.push_str(expanded.trim_start_matches("- "));
        } else if let Some(title) = heading_text(&expanded) {
            // 栏目标题：一级标题（最大号）+ 显式加粗（移动端渲染双保险）。
            out.push_str("# **");
            out.push_str(title);
            out.push_str("**");
        } else {
            out.push_str(&expanded);
        }
    }
    out
}

/// 占位符 → QQ 行内可点标签。
fn expand_cmd_tokens(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    for (token, symbol, command) in cmd_tokens(line) {
        let Some(start) = rest.find(&token) else {
            break;
        };
        out.push_str(&rest[..start]);
        out.push_str(&cmd_input_chip(&symbol, &command));
        rest = &rest[start + token.len()..];
    }
    out.push_str(rest);
    out
}

/// 拼 `<qqbot-cmd-input>` 标签：show 是显示文本，text 是点击后填入输入框的指令；
/// 两个参数按文档要求 urlencode。
fn cmd_input_chip(show: &str, command: &str) -> String {
    format!(
        "<qqbot-cmd-input text=\"{}\" show=\"{}\" reference=\"false\" />",
        url_encode(command),
        url_encode(show)
    )
}

/// percent-encode：保留 RFC 3986 unreserved，其余按 UTF-8 字节转义。
fn url_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// 标题行 → 标题文本（去掉 `#` 前缀与可能的加粗）。
fn heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 {
        return None;
    }
    let rest = &trimmed[hashes..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let rest = rest.strip_prefix("**").unwrap_or(rest);
    Some(rest.strip_suffix("**").unwrap_or(rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ru41_plugin_api::interactive::cmd_token;

    #[test]
    fn expands_cmd_token_into_qq_inline_chip() {
        let line = format!("- {} 板子焊接", cmd_token("☐", "/完成 abc123"));
        let rendered = qq_render(&line);
        assert_eq!(
            rendered,
            "### <qqbot-cmd-input text=\"%2F%E5%AE%8C%E6%88%90%20abc123\" \
             show=\"%E2%98%90\" reference=\"false\" /> 板子焊接"
        );
    }

    #[test]
    fn section_title_becomes_h1_bold() {
        let rendered = qq_render("## 待办（3）");
        assert_eq!(rendered, "# **待办（3）**");
    }

    #[test]
    fn plain_lines_pass_through() {
        assert_eq!(qq_render("- 普通条目"), "- 普通条目");
    }

    #[test]
    fn url_encode_escapes_non_unreserved() {
        assert_eq!(url_encode("/完成 abc123"), "%2F%E5%AE%8C%E6%88%90%20abc123");
        assert_eq!(url_encode("/done a1-b2_c3.4~"), "%2Fdone%20a1-b2_c3.4~");
    }
}
