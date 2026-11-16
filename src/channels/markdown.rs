//! 通用 Markdown 渲染层：**一份规范 → 各网关方言**。
//!
//! 规范层（不在本模块）：
//! - `src/markdown.rs`：模型输出规范（注入提示词尾部）。
//! - `src/util/msg.rs`：外发消息卡片版式（`## 标题` + `- 条目` + `> 指路`）。
//!
//! 本模块只做**渲染适配**：把规范里的通用 Markdown 转成目标网关能渲染的方言，
//! 并对**所有 bot 平台都不支持的能力做统一降级**（表格、公式），而不是
//! 让模型按渠道调整输出。
//!
//! 平台能力事实（2026-11）：bot 平台的 Markdown 交集 = 标题（部分平台降级为
//! 粗体）、列表、粗体、斜体、代码、引用、链接；**表格几乎无人支持**
//! （QQ / Telegram / Slack / Discord / 飞书 / 企微 均不支持），公式同样无人渲染
//! （LaTeX 只对 Obsidian vault 有意义）。所以这两项统一在渲染层兜底。

/// 目标平台的标记方言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Markup {
    /// 平台自己解析 Markdown（QQ 的 `msg_type=2`、Discord、Matrix 等）。
    Markdown,
    /// 平台收 HTML 标签（Telegram `parse_mode=HTML`）。
    Html,
}

/// 一个网关的渲染能力画像：决定通用 Markdown 怎么落到这个平台上。
#[derive(Debug, Clone, Copy)]
pub struct RenderProfile {
    pub id: &'static str,
    pub markup: Markup,
    /// 标题是否保留层级；false → 标题一律降级为粗体。
    pub supports_headings: bool,
    /// 是否支持表格；false → 表格降级为条目列表（两列）或等宽块（多列）。
    pub supports_tables: bool,
    /// 是否渲染公式；false → 公式降级为行内代码。
    pub supports_latex: bool,
    /// 单条消息长度上限（字符）。
    pub max_len: usize,
}

/// 分片预算：留一点余量（HTML 标签与各平台计长方式不同）。
pub const CHUNK_BUDGET: usize = 3900;

/// QQ：平台自带 markdown 子集解析；标题保留，表格/公式不支持。
pub const QQ: RenderProfile = RenderProfile {
    id: "qq",
    markup: Markup::Markdown,
    supports_headings: true,
    supports_tables: false,
    supports_latex: false,
    max_len: 4000,
};

/// Telegram：收 HTML；无多级标题（降级粗体），无表格/公式。
pub const TELEGRAM: RenderProfile = RenderProfile {
    id: "telegram",
    markup: Markup::Html,
    supports_headings: false,
    supports_tables: false,
    supports_latex: false,
    max_len: 4096,
};

/// 把通用 Markdown 渲染成目标方言。
pub fn render(canonical: &str, profile: &RenderProfile) -> String {
    let text = degrade_tables(canonical, profile);
    let text = degrade_latex(&text, profile);
    let mut out = String::new();
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&render_line(line, profile));
    }
    out
}

/// 单行渲染：标题 / 引用 / 列表 / 行内标记。
fn render_line(line: &str, profile: &RenderProfile) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    // 标题：平台不支持层级时降级为粗体。
    for hashes in ["### ", "## ", "# "] {
        if let Some(rest) = trimmed.strip_prefix(hashes) {
            let body = inline(rest, profile);
            return if profile.supports_headings {
                format!("{indent}{hashes}{body}")
            } else {
                format!("{indent}{}", bold(&body, profile))
            };
        }
    }

    // 引用：Markdown 保留 `>`，HTML 用 blockquote。
    if let Some(rest) = trimmed.strip_prefix("> ") {
        let body = inline(rest, profile);
        return match profile.markup {
            Markup::Markdown => format!("{indent}> {body}"),
            Markup::Html => format!("{indent}<blockquote>{body}</blockquote>"),
        };
    }

    // 水平线：HTML 没有 hr，用破折号示意。
    if matches!(trimmed, "---" | "***" | "___") {
        return match profile.markup {
            Markup::Markdown => format!("{indent}{trimmed}"),
            Markup::Html => format!("{indent}———"),
        };
    }

    // 列表：`- ` / `* ` / `1. `；嵌套层级用 ↳ 表示（HTML 会吃掉缩进）。
    if let Some((depth, rest)) = list_item(trimmed) {
        let body = inline(rest, profile);
        let marker = match profile.markup {
            Markup::Markdown => format!("- {body}"),
            Markup::Html => format!("• {body}"),
        };
        let prefix = if depth == 0 {
            String::new()
        } else {
            match profile.markup {
                Markup::Markdown => "  ".repeat(depth),
                Markup::Html => "↳ ".repeat(depth),
            }
        };
        return format!("{indent}{prefix}{marker}");
    }

    format!("{indent}{}", inline(trimmed, profile))
}

/// 解析列表项：返回（缩进层级，正文）。
fn list_item(trimmed: &str) -> Option<(usize, &str)> {
    let depth = trimmed.len() - trimmed.trim_start_matches(' ').len();
    let body = trimmed.trim_start_matches(' ');
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = body.strip_prefix(bullet) {
            return Some((depth / 2, rest));
        }
    }
    // 有序列表：1. 2. …
    let digits = body.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && body[digits..].starts_with(". ") {
        return Some((depth / 2, &body[digits + 2..]));
    }
    None
}

/// 行内标记：按方言转义并转换。
fn inline(text: &str, profile: &RenderProfile) -> String {
    match profile.markup {
        Markup::Markdown => inline_markdown(text),
        Markup::Html => inline_html(text),
    }
}

/// Markdown 方言：只做必要转义，其余原样保留（平台自己解析）。
fn inline_markdown(text: &str) -> String {
    text.to_owned()
}

/// HTML 方言：先转义 HTML 特殊字符，再把 Markdown 行内标记换成标签。
fn inline_html(text: &str) -> String {
    let mut stash: Vec<String> = Vec::new();
    let mut work = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        // 行内代码：整段搬进占位符，避免里面的 * _ 被当强调。
        if chars[index] == '`'
            && let Some(end) = find_char(&chars, index + 1, '`')
        {
            let inner: String = chars[index + 1..end].iter().collect();
            work.push_str(&placeholder(stash.len()));
            stash.push(format!("<code>{}</code>", escape(&inner)));
            index = end + 1;
            continue;
        }
        // 链接：[text](url)
        if chars[index] == '['
            && let Some((label, url, next)) = parse_link(&chars, index)
        {
            work.push_str(&placeholder(stash.len()));
            stash.push(format!(
                "<a href=\"{}\">{}</a>",
                escape(&url),
                inline_html(&label)
            ));
            index = next;
            continue;
        }
        work.push(chars[index]);
        index += 1;
    }
    let escaped = escape(&work);
    let escaped = replace_pair(&escaped, "~~", "<s>", "</s>");
    let escaped = replace_pair(&escaped, "**", "<b>", "</b>");
    let mut escaped = replace_pair(&escaped, "*", "<i>", "</i>");
    for (index, value) in stash.iter().enumerate() {
        escaped = escaped.replace(&placeholder(index), value);
    }
    escaped
}

/// 粗体包装（按方言）。
fn bold(text: &str, profile: &RenderProfile) -> String {
    match profile.markup {
        Markup::Markdown => format!("**{text}**"),
        Markup::Html => format!("<b>{text}</b>"),
    }
}

/// 表格降级：平台不支持表格时改写成条目（两列）或等宽块（多列）。
fn degrade_tables(text: &str, profile: &RenderProfile) -> String {
    if profile.supports_tables {
        return text.to_owned();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let is_header = lines[index].trim_start().starts_with('|');
        let has_separator = lines
            .get(index + 1)
            .is_some_and(|line| is_table_separator(line));
        if is_header && has_separator {
            let mut rows: Vec<Vec<String>> = Vec::new();
            let mut cursor = index;
            while cursor < lines.len() && lines[cursor].trim_start().starts_with('|') {
                if !is_table_separator(lines[cursor]) {
                    rows.push(split_row(lines[cursor]));
                }
                cursor += 1;
            }
            out.push(table_fallback(&rows, profile));
            index = cursor;
            continue;
        }
        out.push(lines[index].to_owned());
        index += 1;
    }
    out.join("\n")
}

/// 表格分隔行（`|---|---|`）。
fn is_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('|') || !trimmed.contains('-') {
        return false;
    }
    trimmed.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
}

/// 拆一行表格为单元格。
fn split_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

/// 表格降级后的表示：一律改成条目列表（与 `util::msg::Card` 的版式一致）。
///
/// 多列表格同样降级成「一行 · 一行」——不搞等宽块：等宽块要么被 HTML 转义
/// 吃掉标签，要么在 QQ 侧还得单独适配；条目列表是所有平台都能渲染的最小交集。
fn table_fallback(rows: &[Vec<String>], _profile: &RenderProfile) -> String {
    let mut out = String::new();
    for row in rows {
        let cells: Vec<&str> = row
            .iter()
            .map(|cell| cell.as_str())
            .filter(|cell| !cell.is_empty())
            .collect();
        if cells.is_empty() {
            continue;
        }
        out.push_str(&format!("- {}\n", cells.join(" · ")));
    }
    out.trim_end().to_owned()
}

/// 公式降级：平台不渲染公式时改成行内代码。
///
/// 只认带 LaTeX 特征（`^ _ \ { }`）的 `$...$`，避免把货币金额 `$5` 误伤。
fn degrade_latex(text: &str, profile: &RenderProfile) -> String {
    if profile.supports_latex {
        return text.to_owned();
    }
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '$'
            && let Some(end) = find_char(&chars, index + 1, '$')
        {
            let inner: String = chars[index + 1..end].iter().collect();
            let looks_like_formula = inner.contains(['^', '_', '\\', '{', '}']);
            if looks_like_formula && !inner.contains('\n') {
                out.push('`');
                out.push_str(&inner);
                out.push('`');
                index = end + 1;
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
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

fn placeholder(index: usize) -> String {
    format!("\u{0}{index}\u{0}")
}

/// 把成对标记换成标签；找不到配对就原样保留。
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

/// HTML 特殊字符转义。
pub fn escape(text: &str) -> String {
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
    fn table_degrades_to_key_value_items() {
        let markdown = "## 标题\n\n| 命令 | 用途 |\n|---|---|\n| `/help` | 查看说明 |\n";
        let html = render(markdown, &TELEGRAM);
        assert!(html.contains("<b>标题</b>"));
        assert!(!html.contains('|'), "表格不该原样漏出：{html}");
        assert!(html.contains("• <code>/help</code> · 查看说明"));
    }

    #[test]
    fn table_degrades_on_qq_too() {
        let markdown = "| 命令 | 用途 |\n|---|---|\n| `/help` | 查看说明 |\n";
        let qq = render(markdown, &QQ);
        assert!(!qq.contains("|---"), "QQ 侧也不该漏表格：{qq}");
        assert!(qq.contains("- `/help` · 查看说明"));
    }

    #[test]
    fn wide_table_becomes_item_list() {
        let markdown = "| a | b | c |\n|---|---|---|\n| 1 | 2 | 3 |\n";
        let html = render(markdown, &TELEGRAM);
        assert!(html.contains("• a · b · c"));
        assert!(html.contains("• 1 · 2 · 3"));
        assert!(!html.contains('|'));
    }

    #[test]
    fn headings_fall_back_to_bold_without_heading_support() {
        let markdown = "### 小节\n";
        assert!(render(markdown, &QQ).contains("### 小节"));
        assert!(render(markdown, &TELEGRAM).contains("<b>小节</b>"));
    }

    #[test]
    fn latex_degrades_but_currency_is_untouched() {
        let formula = "公式 $x^2$ 结束";
        let html = render(formula, &TELEGRAM);
        assert!(html.contains("<code>x^2</code>"));
        assert!(!html.contains('$'));
        let money = "价格 $5 到 $10";
        assert!(render(money, &TELEGRAM).contains("$5 到 $10"));
    }

    #[test]
    fn html_escapes_raw_markup() {
        let html = render("<script>x</script>", &TELEGRAM);
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn markdown_profile_keeps_inline_markers() {
        let md = render("**粗体** 与 `代码`", &QQ);
        assert!(md.contains("**粗体**"));
        assert!(md.contains("`代码`"));
    }

    #[test]
    fn split_respects_structure_boundaries() {
        let text = "第一段\n\n第二段\n\n第三段";
        let chunks = split_markdown(text, 10);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 10));
    }

    #[test]
    fn truncate_bytes_keeps_utf8_boundary() {
        let cut = truncate_bytes("中文内容", 5);
        assert!(cut.len() <= 5);
    }
}
