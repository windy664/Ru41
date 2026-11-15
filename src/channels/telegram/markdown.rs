//! Markdown → Telegram HTML，以及按 Telegram 消息长度上限分片。
//!
//! Telegram 只认 MarkdownV2 / HTML；Ru41 的输出规范是标准 Markdown
//! （`##` 标题、`**粗体**`、列表、代码块、LaTeX），两边不通。
//! 这里做一层窄转换：够用即可，不追求 Telegram 的全部语法。

/// Telegram 单条消息的字符上限。
pub const MAX_MESSAGE_CHARS: usize = 4096;
/// 分片预算：留一点余量给 HTML 标签与平台计长差异。
pub const CHUNK_BUDGET: usize = 3900;

/// Markdown → Telegram HTML（`parse_mode=HTML`）。
pub fn to_html(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    let mut code_lines: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_code {
                out.push_str("<pre>");
                for code in &code_lines {
                    out.push_str(&escape(code));
                    out.push('\n');
                }
                out.push_str("</pre>");
                code_lines.clear();
                in_code = false;
            } else {
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_lines.push(line);
            continue;
        }
        out.push_str(&line_to_html(line));
        out.push('\n');
    }
    // 未闭合的代码块：照样按代码块输出，别把内容当普通文本排版。
    if in_code {
        out.push_str("<pre>");
        for code in &code_lines {
            out.push_str(&escape(code));
            out.push('\n');
        }
        out.push_str("</pre>");
    }
    out.trim_end().to_owned()
}

/// 单行 Markdown → HTML：一行一个块，便于 Telegram 正常换行。
fn line_to_html(line: &str) -> String {
    let escaped = escape(line);
    let trimmed = escaped.trim_start();
    let indent_len = escaped.len() - trimmed.len();
    let indent = &escaped[..indent_len];
    // 标题：Telegram 没有多级标题，统一降级成粗体。
    for prefix in ["### ", "## ", "# "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return format!("{indent}<b>{}</b>", inline(rest));
        }
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        return format!("{indent}<blockquote>{}</blockquote>", inline(rest));
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            return format!("{indent}• {}", inline(rest));
        }
    }
    format!("{indent}{}", inline(trimmed))
}

/// 行内标记：行内代码 / 链接先抽成占位符，再处理强调。
fn inline(text: &str) -> String {
    let mut stash: Vec<String> = Vec::new();
    let mut work = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`'
            && let Some(end) = find_char(&chars, index + 1, '`')
        {
            let inner: String = chars[index + 1..end].iter().collect();
            work.push_str(&placeholder(stash.len()));
            stash.push(format!("<code>{inner}</code>"));
            index = end + 1;
            continue;
        }
        if chars[index] == '['
            && let Some((label, url, next)) = parse_link(&chars, index)
        {
            work.push_str(&placeholder(stash.len()));
            stash.push(format!("<a href=\"{url}\">{label}</a>"));
            index = next;
            continue;
        }
        work.push(chars[index]);
        index += 1;
    }
    work = replace_pair(&work, "**", "<b>", "</b>");
    work = replace_pair(&work, "*", "<i>", "</i>");
    for (index, value) in stash.iter().enumerate() {
        work = work.replace(&placeholder(index), value);
    }
    work
}

fn placeholder(index: usize) -> String {
    format!("\u{0}{index}\u{0}")
}

fn find_char(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|index| chars[*index] == target)
}

/// 解析 `[label](url)`，返回标签、地址与结束位置。
fn parse_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut index = start + 1;
    let mut label = String::new();
    while index < chars.len() && chars[index] != ']' {
        label.push(chars[index]);
        index += 1;
    }
    if index >= chars.len() || chars[index] != ']' {
        return None;
    }
    index += 1;
    if index >= chars.len() || chars[index] != '(' {
        return None;
    }
    index += 1;
    let mut url = String::new();
    while index < chars.len() && chars[index] != ')' {
        url.push(chars[index]);
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }
    Some((label, url, index + 1))
}

/// 把成对标记换成 HTML 标签；找不到配对就原样保留。
fn replace_pair(text: &str, marker: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        let Some(end) = after.find(marker) else { break };
        let inner = &after[..end];
        out.push_str(&rest[..start]);
        if inner.is_empty() {
            out.push_str(marker);
        } else {
            out.push_str(open);
            out.push_str(inner);
            out.push_str(close);
        }
        rest = &after[end + marker.len()..];
    }
    out.push_str(rest);
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 按消息长度上限切 Markdown：优先段落边界，其次行边界，最后硬切。
pub fn split_markdown(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![text.to_owned()];
    }
    if text.chars().count() <= limit {
        return vec![text.to_owned()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for paragraph in text.split("\n\n") {
        let candidate = current.chars().count() + paragraph.chars().count() + 2;
        if !current.is_empty() && candidate > limit {
            chunks.push(std::mem::take(&mut current));
        }
        if paragraph.chars().count() > limit {
            for line in paragraph.split('\n') {
                let candidate = current.chars().count() + line.chars().count() + 1;
                if !current.is_empty() && candidate > limit {
                    chunks.push(std::mem::take(&mut current));
                }
                if line.chars().count() > limit {
                    chunks.push(line.chars().take(limit).collect());
                } else {
                    if !current.is_empty() {
                        current.push('\n');
                    }
                    current.push_str(line);
                }
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(paragraph);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// 按字节上限截断（不切碎 UTF-8）。Telegram 的 callback_data 上限 64 字节。
pub fn truncate_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_headings_bold_lists_and_code() {
        let html = to_html("## 任务完成\n**重点**：结果\n- 第一项\n- 第二项");
        assert!(html.contains("<b>任务完成</b>"));
        assert!(html.contains("<b>重点</b>"));
        assert!(html.contains("• 第一项"));
        assert!(html.contains("• 第二项"));
    }

    #[test]
    fn code_blocks_are_escaped_and_not_reformatted() {
        let html = to_html("```rust\nlet x = a < b && c > d;\n```");
        assert!(html.contains("<pre>"));
        assert!(html.contains("a &lt; b &amp;&amp; c &gt; d"));
        assert!(!html.contains("**"));
    }

    #[test]
    fn inline_code_and_links_survive_emphasis() {
        let html = to_html("看 `a*b*c` 与 [文档](https://example.com/x)");
        assert!(html.contains("<code>a*b*c</code>"));
        assert!(html.contains("<a href=\"https://example.com/x\">文档</a>"));
    }

    #[test]
    fn escapes_raw_html_from_model_output() {
        let html = to_html("<script>alert(1)</script>");
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn splits_long_text_on_paragraph_boundaries() {
        let text = "第一段内容。\n\n第二段内容。\n\n第三段内容。";
        let chunks = split_markdown(text, 12);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 12));
        assert_eq!(
            chunks.concat().replace("\n\n", ""),
            text.replace("\n\n", "")
        );
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(split_markdown("很短", 100), vec!["很短".to_owned()]);
    }

    #[test]
    fn truncate_bytes_keeps_utf8_boundary() {
        let cut = truncate_bytes("中文内容", 5);
        assert!(cut.len() <= 5);
        assert!(cut.chars().count() >= 1);
    }
}
