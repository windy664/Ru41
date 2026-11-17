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
    // 有没有一级标题，决定栏目标题怎么处理：
    // - **长文推送**（`#` 大标题 + `##` 小节 + 正文）：按原层级渲染。
    //   标题最大号、小节居中、正文普通，层次才分得开；全拍平成一级标题的话，
    //   六个小节标题跟文档标题一样大，正文夹在中间很违和。
    // - **待办清单**（只有一个 `##` 栏目标题 + 一堆可点条目）：提升成一级标题 +
    //   加粗，条目走三级标题——待办视图要的就是「标题最大」。
    let has_h1 = text.lines().any(|line| heading_level(line) == Some(1));
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
            match heading_level(&expanded) {
                Some(level) if has_h1 => {
                    // 长文：保留原层级，标题层级本身就是字号。
                    out.push_str(&"#".repeat(level));
                    out.push(' ');
                    out.push_str(title);
                }
                _ => {
                    // 待办式：栏目标题提升成一级标题（最大号）+ 显式加粗（移动端渲染双保险）。
                    out.push_str("# **");
                    out.push_str(title);
                    out.push_str("**");
                }
            }
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
    let hashes = heading_level(line)?;
    let trimmed = line.trim_start();
    let rest = &trimmed[hashes..];
    let rest = rest.strip_prefix(' ').unwrap_or(rest);
    let rest = rest.strip_prefix("**").unwrap_or(rest);
    Some(rest.strip_suffix("**").unwrap_or(rest))
}

/// 标题层级（1..=6）；不是标题行返回 None。
///
/// ATX 标题要求 `#` 后面跟空格——和通用渲染器（`channels::markdown`）的判定一致。
/// 「#板子焊接」是待办标题**文本**（用户用 `#` 立项），不是标题行；
/// 当成标题处理会让整条待办消息误判成长文，待办标题反而变小。
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let hashes = trimmed.len() - trimmed.trim_start_matches('#').len();
    if hashes == 0 {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some(hashes)
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
    fn long_form_keeps_heading_levels() {
        // 长文推送：文档标题最大、小节居中、正文普通——层次要分得开。
        let text = "# 近期 CTF / 网络安全竞赛\n## 一、当前正在报名\nCISCN 创新实践能力赛";
        let rendered = qq_render(text);
        assert_eq!(
            rendered,
            "# 近期 CTF / 网络安全竞赛\n## 一、当前正在报名\nCISCN 创新实践能力赛"
        );
    }

    #[test]
    fn hash_prefixed_task_title_is_not_a_heading() {
        // 「#板子焊接」是待办标题文本，不是 markdown 标题：不能当成标题，
        // 更不能因为它让整条消息误判成长文。
        let rendered = qq_render("## 待办（2）\n- #板子焊接");
        assert_eq!(rendered, "# **待办（2）**\n- #板子焊接");
    }

    #[test]
    fn long_form_keeps_deeper_levels_too() {
        let rendered = qq_render("# 标题\n### 更深一层");
        assert_eq!(rendered, "# 标题\n### 更深一层");
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
