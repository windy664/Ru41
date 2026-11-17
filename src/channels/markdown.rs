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
    /// 是否支持行内可点指令（QQ 的 `<qqbot-cmd-input>`）；false → 占位符降级为纯符号。
    pub supports_cmd_input: bool,
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
    supports_cmd_input: true,
    max_len: 4000,
};

/// Telegram：收 HTML；无多级标题（降级粗体），无表格/公式。
pub const TELEGRAM: RenderProfile = RenderProfile {
    id: "telegram",
    markup: Markup::Html,
    supports_headings: false,
    supports_tables: false,
    supports_latex: false,
    supports_cmd_input: false,
    max_len: 4096,
};

/// 占位符 → 链接的构造器（由网关提供）：命令文本 → 链接地址。
/// 返回 None 表示造不出链接（超长等），此时退回纯符号。
pub type CmdLinkBuilder<'a> = &'a dyn Fn(&str) -> Option<String>;

/// 把通用 Markdown 渲染成目标方言。
pub fn render(canonical: &str, profile: &RenderProfile) -> String {
    render_inner(canonical, profile, None)
}

/// 带**行内可点链接**的渲染：占位符展开成正文里的可点元素。
///
/// 链接目标由网关给（`link_for`：命令文本 → 链接），平台专有细节不进本层——
/// Telegram 用 `tg://` 深链，QQ 走 `channels::qq::render` 的 cmd-input 芯片。
pub fn render_with_cmd_links(
    canonical: &str,
    profile: &RenderProfile,
    link_for: CmdLinkBuilder<'_>,
) -> String {
    render_inner(canonical, profile, Some(link_for))
}

fn render_inner(
    canonical: &str,
    profile: &RenderProfile,
    link_for: Option<CmdLinkBuilder<'_>>,
) -> String {
    // 不支持行内可点指令的渠道：占位符降级成纯符号（永不漏 token）；
    // 网关给了链接目标时，占位符展开成正文里的可点链接。
    let canonical = match (profile.supports_cmd_input, link_for) {
        (true, _) => canonical.to_owned(),
        (false, Some(link_for)) => expand_cmd_tokens_as_links(canonical, link_for),
        (false, None) => ru41_plugin_api::interactive::strip_cmd_tokens(canonical),
    };
    let canonical = canonical.as_str();
    let text = degrade_tables(canonical, profile);
    let text = degrade_latex(&text, profile);
    render_body(&text, profile)
}

/// 占位符 → 正文里的可点链接：`{{cmd:符号|指令}}` → `[符号](链接)`。
///
/// 借用 Markdown 链接语法，后面的 Markdown→HTML 流程会把它转成 `<a href>`，
/// 属性转义也随之复用（`&` → `&amp;`）。
fn expand_cmd_tokens_as_links(text: &str, link_for: CmdLinkBuilder<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    for (token, symbol, command) in ru41_plugin_api::interactive::cmd_tokens(text) {
        let Some(start) = rest.find(&token) else {
            break;
        };
        out.push_str(&rest[..start]);
        match link_for(&command) {
            // 链接造不出来（比如超长）→ 退回纯符号，不能留空 href。
            Some(href) => {
                out.push('[');
                out.push_str(&symbol);
                out.push_str("](");
                out.push_str(&href);
                out.push(')');
            }
            None => out.push_str(&symbol),
        }
        rest = &rest[start + token.len()..];
    }
    out.push_str(rest);
    out
}

/// 正文渲染：围栏代码块是**原子单元**（整体处理，不逐行当列表/标题），
/// 其余按行渲染。
fn render_body(text: &str, profile: &RenderProfile) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if fence_open(lines[index]).is_some() {
            let mut body: Vec<&str> = Vec::new();
            index += 1;
            while index < lines.len() && !is_fence_close(lines[index]) {
                body.push(lines[index]);
                index += 1;
            }
            // 闭合围栏（如果有）跳过。
            if index < lines.len() {
                index += 1;
            }
            pieces.push(render_fence(&body, profile));
            continue;
        }
        pieces.push(render_line(lines[index], profile));
        index += 1;
    }
    pieces.join("\n")
}

/// 围栏代码块：HTML 方言落成 `<pre>`（等宽、保空白），Markdown 方言落成裸正文
/// （QQ 的 markdown 子集没有围栏语法，留着 ``` 只会露出标记）。
///
/// 围栏内的内容**不再走行内标记**——代码里的 `*`、`_`、`[x](y)` 都是字面量。
fn render_fence(body: &[&str], profile: &RenderProfile) -> String {
    match profile.markup {
        Markup::Html => {
            let mut out = String::from("<pre>");
            for (index, line) in body.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str(&escape(line));
            }
            out.push_str("</pre>");
            out
        }
        Markup::Markdown => body.join("\n"),
    }
}

/// 围栏开始行 → 语言标注（` ```python ` → `python`）；不是围栏则 None。
fn fence_open(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("```")?;
    Some(rest.trim().to_owned())
}

/// 围栏结束行：只有 ```（可带缩进），后面不能再跟语言标注。
fn is_fence_close(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("```") && trimmed.trim_start_matches('`').trim().is_empty()
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
                escape_attr(&url),
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

/// 属性值转义：比正文多转义引号——URL 落在 `href="…"` 里，
/// 一个裸引号就能把属性撑破（正文里的引号不需要转义）。
pub fn escape_attr(text: &str) -> String {
    escape(text).replace('"', "&quot;")
}

/// UTF-16 码元长度：Telegram 按 UTF-16 码元计长（星平面字符占 2 个），
/// 用字符数算会低估——一屏 emoji 就能把 4096 的上限顶穿。
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// 按消息长度上限切 Markdown：段落边界 → 行边界 → 硬切。
///
/// **围栏代码块是原子单元**：不会从中间切开；实在太长时按行切，并用同一段
/// 语言标注在下一片重新开围栏（否则 Telegram 会把后半截当普通文本）。
///
/// 长度按 **UTF-16 码元**计（Telegram 的口径），不是字符数。
pub fn split_markdown(text: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return vec![text.to_owned()];
    }
    if utf16_len(text) <= limit {
        return vec![text.to_owned()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for unit in split_units(text) {
        for piece in split_unit(&unit, limit) {
            let extra = if current.is_empty() {
                utf16_len(&piece)
            } else {
                utf16_len(&current) + 1 + utf16_len(&piece)
            };
            if !current.is_empty() && extra > limit {
                chunks.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(&piece);
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

/// 切分单元：段落（空行分隔）与围栏代码块各自成组，不互相拆散。
fn split_units(text: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut units: Vec<Vec<String>> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if fence_open(line).is_some() {
            if !paragraph.is_empty() {
                units.push(std::mem::take(&mut paragraph));
            }
            let mut block = vec![line.to_owned()];
            index += 1;
            while index < lines.len() {
                let closing = is_fence_close(lines[index]);
                block.push(lines[index].to_owned());
                index += 1;
                if closing {
                    break;
                }
            }
            units.push(block);
            continue;
        }
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                units.push(std::mem::take(&mut paragraph));
            }
            index += 1;
            continue;
        }
        paragraph.push(line.to_owned());
        index += 1;
    }
    if !paragraph.is_empty() {
        units.push(paragraph);
    }
    units
}

/// 一个单元切成若干片，每片自身不超限。
fn split_unit(unit: &[String], limit: usize) -> Vec<String> {
    let joined_len = utf16_len(&unit.join("\n"));
    if joined_len <= limit {
        return vec![unit.join("\n")];
    }
    let is_fence = unit.first().is_some_and(|line| fence_open(line).is_some());
    if is_fence {
        return split_fence(unit, limit);
    }
    // 普通段落：按行贪心装片。
    let mut pieces: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_len = 0usize;
    for line in unit {
        let line_len = utf16_len(line) + 1;
        if !current.is_empty() && current_len + line_len > limit {
            pieces.push(current.join("\n"));
            current = Vec::new();
            current_len = 0;
        }
        if line_len > limit {
            // 单行就超限：硬切（按 UTF-16 码元，不切碎字符）。
            for part in hard_split(line, limit) {
                pieces.push(part);
            }
            continue;
        }
        current.push(line.clone());
        current_len += line_len;
    }
    if !current.is_empty() {
        pieces.push(current.join("\n"));
    }
    pieces
}

/// 围栏块超长：按行切，每片都用同一段语言标注重新开围栏并补上闭合。
fn split_fence(unit: &[String], limit: usize) -> Vec<String> {
    let open = unit.first().map(String::as_str).unwrap_or("");
    let info = fence_open(open).unwrap_or_default();
    let closing = unit.last().is_some_and(|line| is_fence_close(line));
    let body: &[String] = if closing && unit.len() > 1 {
        &unit[1..unit.len() - 1]
    } else {
        &unit[1..]
    };
    // 每片的固定开销：开围栏 + 换行 + 闭合围栏。
    let overhead = utf16_len(&format!("```{info}")) + 1 + 3;
    let budget = limit.saturating_sub(overhead).max(1);
    let mut pieces: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_len = 0usize;
    let flush = |current: &mut Vec<String>, pieces: &mut Vec<String>| {
        if current.is_empty() {
            return;
        }
        let mut piece = format!("```{info}");
        for line in current.iter() {
            piece.push('\n');
            piece.push_str(line);
        }
        piece.push_str("\n```");
        pieces.push(piece);
        current.clear();
    };
    for line in body {
        let line_len = utf16_len(line) + 1;
        if !current.is_empty() && current_len + line_len > budget {
            flush(&mut current, &mut pieces);
            current_len = 0;
        }
        if line_len > budget {
            flush(&mut current, &mut pieces);
            for part in hard_split(line, budget) {
                pieces.push(format!("```{info}\n{part}\n```"));
            }
            current_len = 0;
            continue;
        }
        current.push(line.clone());
        current_len += line_len;
    }
    flush(&mut current, &mut pieces);
    if pieces.is_empty() {
        pieces.push(format!("```{info}\n```"));
    }
    pieces
}

/// 按 UTF-16 码元硬切（不切碎字符；代理对不拆开——Rust 的 char 已是完整码点）。
fn hard_split(text: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for ch in text.chars() {
        let width = ch.len_utf16();
        if len + width > limit && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            len = 0;
        }
        current.push(ch);
        len += width;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
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
    fn fenced_code_becomes_pre_block() {
        let md = "```python\nprint('x')\n```";
        let rendered = render(md, &TELEGRAM);
        assert_eq!(rendered, "<pre>print('x')</pre>");
        // 不残留裸反引号，也不被拆成两个空 inline code。
        assert!(!rendered.contains('`'), "{rendered}");
    }

    #[test]
    fn fence_body_keeps_markdown_literal() {
        // 代码里的 * _ [x](y) 都是字面量，不能被当成强调/链接。
        let md = "```\na *b* _c_ [d](e)\n```";
        let rendered = render(md, &TELEGRAM);
        assert_eq!(rendered, "<pre>a *b* _c_ [d](e)</pre>");
    }

    #[test]
    fn qq_profile_drops_fence_markers_but_keeps_body() {
        let md = "```python\nprint('x')\n```";
        let rendered = render(md, &QQ);
        assert_eq!(rendered, "print('x')");
    }

    #[test]
    fn unclosed_fence_does_not_leak_markers() {
        let md = "```python\nprint('never closed')";
        let rendered = render(md, &TELEGRAM);
        assert_eq!(rendered, "<pre>print('never closed')</pre>");
    }

    #[test]
    fn split_never_cuts_inside_a_fence() {
        let mut md = String::from("```python\n");
        for index in 0..40 {
            md.push_str(&format!("line_{index}\n"));
        }
        md.push_str("```\n");
        let chunks = split_markdown(&md, 120);
        assert!(chunks.len() > 1, "{chunks:?}");
        for chunk in &chunks {
            let opens = chunk.matches("```").count();
            assert_eq!(opens % 2, 0, "围栏不成对：{chunk:?}");
            // 每片都是完整的围栏块。
            assert!(chunk.trim_start().starts_with("```"), "{chunk:?}");
            assert!(chunk.trim_end().ends_with("```"), "{chunk:?}");
            // 语言标注在每片都保留。
            assert!(chunk.starts_with("```python"), "{chunk:?}");
        }
    }

    #[test]
    fn length_counts_utf16_code_units() {
        // 星平面字符占 2 个码元。
        assert_eq!(utf16_len("😀"), 2);
        assert_eq!(utf16_len("ab😀"), 4);
        // 一屏 emoji 的字符数远小于 Telegram 的计长口径。
        let text = "😀".repeat(10);
        assert_eq!(text.chars().count(), 10);
        assert_eq!(utf16_len(&text), 20);
    }

    #[test]
    fn split_respects_utf16_budget_with_emoji() {
        let text = "😀".repeat(50);
        let chunks = split_markdown(&text, 40);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(utf16_len(chunk) <= 40, "分片超限：{}", utf16_len(chunk));
        }
    }

    #[test]
    fn link_url_cannot_break_out_of_attribute() {
        let rendered = render(r#"[点我](x" onmouseover="alert(1))"#, &TELEGRAM);
        assert!(!rendered.contains(r#"onmouseover=""#), "{rendered}");
        assert!(rendered.contains("&quot;"), "{rendered}");
    }

    #[test]
    fn table_never_reaches_any_channel() {
        let md = "| 命令 | 用途 |\n|---|---|\n| `/` | 帮助 |";
        for profile in [QQ, TELEGRAM] {
            let rendered = render(md, &profile);
            assert!(!rendered.contains("|---"), "{profile:?}: {rendered}");
        }
    }

    #[test]
    fn cmd_token_can_become_an_inline_link() {
        let md = "- {{cmd:☐|/完成 abc123}} 板子焊接";
        let rendered = render_with_cmd_links(md, &TELEGRAM, &|command| {
            Some(format!("tg://resolve?domain=Ru41bot&start={command}"))
        });
        // 复选框本身变成可点链接，标题照旧。
        assert!(
            rendered
                .contains("<a href=\"tg://resolve?domain=Ru41bot&amp;start=/完成 abc123\">☐</a>"),
            "{rendered}"
        );
        assert!(rendered.contains("• <a href="), "{rendered}");
        assert!(rendered.contains("板子焊接"), "{rendered}");
        assert!(!rendered.contains("{{cmd:"), "{rendered}");
    }

    #[test]
    fn link_target_too_long_falls_back_to_plain_symbol() {
        let md = "- {{cmd:☐|/完成 abc123}} 板子焊接";
        let rendered = render_with_cmd_links(md, &TELEGRAM, &|_| None);
        assert!(rendered.contains("• ☐ 板子焊接"), "{rendered}");
        assert!(!rendered.contains("<a href"), "{rendered}");
    }
}
