use crate::{
    domain::{AgentResult, MAX_RESULT_MEDIA, error::Ru41Error},
    ports::channel::{MediaKind, MediaRef},
};
use anyhow::Result;
use serde_json::Value;

/// 从模型输出里取出**契约对象**（带 summary 字段的 JSON）。
///
/// 模型常把契约 JSON 附在散文后面（2026-11-09 课堂笔记就是：先写「本次笔记要点」，
/// 末尾再补一段 `{"summary": ...}`）。这时只认「带 summary 的那个对象」——
/// 散文里的括号不该把契约 JSON 抢掉。
fn extract_contract_json(text: &str) -> Option<(&str, String)> {
    let mut offset = 0usize;
    while let Some(found) = text[offset..].find('{') {
        let start = offset + found;
        let Some(end) = balanced_object_end(text, start) else {
            offset = start + 1;
            continue;
        };
        let candidate = &text[start..=end];
        if let Ok(value) = serde_json::from_str::<Value>(candidate)
            && let Some(summary) = value.get("summary").and_then(|v| v.as_str())
        {
            return Some((candidate, unwrap_summary(summary)));
        }
        offset = end + 1;
    }
    None
}

/// 契约里的 summary 可能**又被包了一层** ```json 围栏
/// （2026-11-18 课堂笔记实测：模型输出
/// `{"summary": "```json\n{\"summary\": \"## 本次笔记要点…\"}\n```"}`）。
/// 那时直接投递会把**原始 JSON** 发给用户（用户看到的是一坨带转义的 JSON）。
/// 这里先剥围栏、再取内层的 summary。
///
/// 但要保守：围栏之外还有正经散文 → 那是报告里**引用**的代码片段，不能动。
fn unwrap_summary(summary: &str) -> String {
    let mut current = summary.trim().to_owned();
    // 最多剥三层，防自我嵌套。
    for _ in 0..3 {
        let Some(json) = fenced_json(&current) else {
            break;
        };
        // 围栏之外还剩下一堆正文 → 说明这是报告里引用的片段，别剥。
        if current.replace(json, "").chars().count() > 20 {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(json) else {
            break;
        };
        let Some(inner) = value.get("summary").and_then(|v| v.as_str()) else {
            break;
        };
        let inner = inner.trim().to_owned();
        if inner.is_empty() {
            break;
        }
        current = inner;
    }
    current
}

/// 取 ```json … ``` 围栏里的 JSON 文本（语言标记 json/javascript 都认）。
fn fenced_json(text: &str) -> Option<&str> {
    let start = text.find("```")?;
    let after = text[start + 3..].trim_start();
    // 围栏的语言标记（json / javascript…）到第一个空白为止
    let idx = after.find(char::is_whitespace)?;
    let after = after[idx..].trim_start();
    let end = after.find("```")?;
    Some(after[..end].trim())
}

/// 从 `start`（一个 `{`）开始找配平的 `}`，返回它的位置。
fn balanced_object_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        let ch = *byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// 从模型输出里取出 JSON 对象。
/// Pi 没有 codex 那样的输出 schema 强制，模型常把 JSON 包在 ```json 代码块里
/// 或前后加说明；这里取第一个配平的 `{...}` 再解析，别让格式噪声废掉整次运行。
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, byte) in bytes.iter().enumerate().skip(start) {
        let ch = *byte as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 取一条 assistant 消息里的全部文本块。
pub(super) fn message_text(message: &Value) -> String {
    let mut text = String::new();
    for block in message["content"].as_array().into_iter().flatten() {
        if block["type"] == "text"
            && let Some(chunk) = block["text"].as_str()
        {
            text.push_str(chunk);
        }
    }
    strip_thinking(&text).to_owned()
}

/// 剥掉**思考段**，只留真正的交付内容。
///
/// 2026-11-23 实测（MiniMax-M3 / anthropic-messages 接口）：模型把思考过程和
/// 契约 JSON 放在**同一个 text block**里：
///
/// ```text
/// I'll evaluate these candidates ... Write it out.</think>{"summary": "# AI / CS 日报…"}
/// ```
///
/// 闭标记里夹着 **U+200B 零宽字符**（`</` + U+200B + `mm:think>`）——等宽匹配
/// 剥不掉，于是整段思考被当成正文候选；思考又比真正的日报长 → 契约层
/// 「散文更长就以散文为准」的兜底把**思考过程**推给了用户（11-22、11-23 的日报）。
///
/// 规则：
/// - 有闭标记 → 闭标记**之前**全是思考，丢掉（开标记有时已被适配器剥掉）；
/// - 只有开标记 → 思考到结尾，后面没有正文，返回空（让契约层判「没正文」重试，
///   好过把思考发出去）；
/// - 都没有 → 原样返回。
fn strip_thinking(text: &str) -> &str {
    static CLOSE_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let close_re = CLOSE_RE.get_or_init(|| {
        regex::Regex::new(r"</[\u{200b}\u{200c}\u{200d}\u{feff}\u{2060}]*(?:mm:)?think>")
            .expect("闭标记正则")
    });
    if let Some(matched) = close_re.find(text) {
        return &text[matched.end()..];
    }
    static OPEN_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let open_re =
        OPEN_RE.get_or_init(|| regex::Regex::new(r"<\u{feff}?think").expect("开标记正则"));
    if open_re.is_match(text) {
        // 只有开标记：思考一直延续到结尾，后面没有正文 → 交白卷
        // （上层会判「没有正文」让任务重试，好过把思考推给用户）
        return "";
    }
    text
}

/// 挑出「完整正文」候选：模型把报告写在前面一条消息里，最后一条 JSON 的
/// summary 反而是压缩版时，返回那份正文（2026-11-06 晚报实测）。
///
/// 判定要保守：正文得明显更长（2 倍以上）且本身就是一份像样的交付物，
/// 否则宁可交付 summary——压缩摘要也是模型自己的选择。
pub(super) fn pick_full_report(candidates: &[String], summary: &str) -> Option<String> {
    let summary_len = summary.trim().chars().count();
    let best = candidates
        .iter()
        .filter(|c| {
            c.trim().chars().count() > summary_len.saturating_mul(2)
                && c.trim().chars().count() > 800
        })
        .max_by_key(|c| c.trim().chars().count())?;
    // 正文本身得像交付物：带 Markdown 标题或分节，否则可能只是过程絮叨。
    let looks_like_report = best.starts_with('#')
        || best.contains("\n## ")
        || best.contains("\n- **")
        || best.contains("\n### ");
    if !looks_like_report {
        return None;
    }
    // 报告候选也可能是「正文 + 契约 JSON」：这里同样剥掉信封，
    // 否则 parse_agent_result 已清理的最终消息会被这个兜底重新弄脏。
    let body = match extract_contract_json(best) {
        Some((contract, summary)) => {
            let prose = clean_contract_prose(&best.replace(contract, ""));
            if prose.chars().count() >= 20 {
                prose
            } else {
                summary
            }
        }
        None => best.trim().to_owned(),
    };
    Some(body)
}

/// 散文是否像一份交付物：有 Markdown 结构（标题、列表、表格）才算。
/// 一句「我已拿到所需信息」不是交付物，只是模型的过场话。
fn looks_like_deliverable(text: &str) -> bool {
    text.contains("\n#")
        || text.contains("\n- ")
        || text.contains("\n* ")
        || text.contains("\n1. ")
        || text.contains("|---")
        || text.contains("\n**")
}

/// 契约 JSON 只是信封：挖掉它以后，把包它的 Markdown 代码围栏与前置分隔线也去掉。
/// 不能只 trim 末尾的 ```——那会把开头的 ```json 留成用户可见的尾巴。
fn clean_contract_prose(text: &str) -> String {
    fn is_fence(line: &str) -> bool {
        line.trim().strip_prefix("```").is_some_and(|lang| {
            lang.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        })
    }
    let mut lines: Vec<&str> = text.trim().lines().collect();
    while lines.first().is_some_and(|line| is_fence(line)) {
        lines.remove(0);
    }
    // 围栏之间往往隔着空行，所以空行、围栏、分隔线要交替清理到稳定为止。
    loop {
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        if lines
            .last()
            .is_some_and(|line| is_fence(line) || line.trim() == "---")
        {
            lines.pop();
            continue;
        }
        break;
    }
    lines.join("\n").trim().to_owned()
}

/// 认出「机器消费的指令围栏」交付物。
///
/// 底座交回的是一段围栏（```ru41-organize …```），里面装的是给宿主/插件
/// **执行**的指令，没有 `summary` 字段——这是契约如此，不是模型跑偏。
/// 2026-11-22 实测：收件箱整理任务因此被判「Pi result must be JSON with
/// summary」，重试 9 次后放弃，四个文件一直躺在收件箱里。
///
/// 认的条件：围栏语言已登记在 `domain::fence::INSTRUCTION_FENCES`，且围栏正文
/// 是一份 JSON 对象。没登记的围栏（论文日报的 ru41-screen 等）仍按原口径判契约不符。
fn instruction_fence_payload(text: &str) -> Option<String> {
    let fence_start = text.find("```")?;
    let after = &text[fence_start + 3..];
    let lang_end = after.find(char::is_whitespace)?;
    let lang = after[..lang_end].trim();
    if !crate::domain::fence::INSTRUCTION_FENCES.contains(&lang) {
        return None;
    }
    let rest = &after[lang_end..];
    let body_rel = rest.trim_start();
    let body_end = body_rel.find("```")?;
    let body = body_rel[..body_end].trim();
    let value = serde_json::from_str::<Value>(body).ok()?;
    if !value.is_object() {
        return None;
    }
    let body_abs = fence_start + 3 + lang_end + (rest.len() - body_rel.len());
    let close = body_abs + body_end;
    // 后面还有别的围栏 → 不是单份指令交付物，交给原口径。
    if text[close + 3..].contains("```") {
        return None;
    }
    Some(text[fence_start..close + 3].trim().to_owned())
}

/// 交付物是不是「机器消费的指令围栏」——这类交付物不许被正文兜底替换：
/// 指令围栏是给插件执行的，正文兜底会把指令换成一段叙述，插件就解析不到了。
pub(super) fn is_instruction_deliverable(text: &str) -> bool {
    instruction_fence_payload(text.trim()).is_some()
}

pub(super) fn parse_agent_result(text: &str) -> Result<AgentResult> {
    // 先剥思考段（MiniMax-M3 把思考和契约 JSON 放在同一个 text block 里，
    // 2026-11-23 实测）—— 不剥的话「散文更长就以散文为准」的兜底会把
    // 思考过程当成交付物推给用户。
    let text = strip_thinking(text);
    let trimmed = text.trim();
    // 「机器消费的指令围栏」：交付物是给宿主/插件执行的指令（ru41-organize 等），
    // 没有 summary 是契约如此——收下整段围栏，别判契约不符让任务重试。
    if let Some(payload) = instruction_fence_payload(trimmed) {
        let result = AgentResult {
            summary: payload,
            media: Vec::new(),
        };
        result.validate().map_err(Ru41Error::InvalidOutput)?;
        return Ok(result);
    }
    // 模型常在散文后面附一段契约 JSON（课堂笔记实测：先写要点，末尾再补
    // `{"summary": ...}`）。契约对象取「带 summary 的那个」，不是「第一个 {」。
    if let Some((contract, summary)) = extract_contract_json(trimmed) {
        let media = serde_json::from_str::<Value>(contract)
            .map(|value| parse_media_list(value.get("media")))
            .unwrap_or_default();
        // JSON 之外可能还有正文，但那不一定是交付物：模型常先写一句
        // 「我已拿到所需信息」再把完整答案放进契约 JSON（2026-11-12 港澳出行问答
        // 实测：散文 150 字符的过场话覆盖了 4296 字符的真答案）。
        // 只有散文本身像交付物（或比 summary 还长），才让散文优先；
        // 否则一律交付契约里的 summary。
        let prose = clean_contract_prose(&trimmed.replace(contract, ""));
        let prose_len = prose.chars().count();
        let summary_len = summary.chars().count();
        let summary =
            if prose_len >= 20 && (looks_like_deliverable(&prose) || prose_len >= summary_len) {
                prose
            } else {
                summary
            };
        let result = AgentResult { summary, media };
        result.validate().map_err(Ru41Error::InvalidOutput)?;
        return Ok(result);
    }
    let candidate = extract_json_object(trimmed).unwrap_or(trimmed);
    // 先按值解析再挑字段：底座给的 media 可能是字符串路径，也可能是对象，
    // 直接反序列化 AgentResult 遇到不认的形状会整条判失败——那就白跑一次。
    if let Ok(value) = serde_json::from_str::<Value>(candidate)
        && let Some(summary) = value.get("summary").and_then(|v| v.as_str())
    {
        let result = AgentResult {
            summary: summary.to_owned(),
            media: parse_media_list(value.get("media")),
        };
        result.validate().map_err(Ru41Error::InvalidOutput)?;
        return Ok(result);
    }
    // 模型交了一份 JSON，但没有 summary 字段（2026-11-09 论文日报实测：
    // 整篇筛选结果被包成 ```ru41-screen 代码块直接推给了用户）。
    // 这类输出不是交付物：JSON 之外还有正经正文就取正文，否则判契约不符
    // 让任务重试——宁可这一轮没内容，也不要把原始 JSON 推给用户。
    if let Ok(value) = serde_json::from_str::<Value>(candidate)
        && value.is_object()
    {
        let prose = clean_contract_prose(&trimmed.replace(candidate, ""));
        if prose.chars().count() >= 20 {
            let result = AgentResult {
                summary: prose,
                media: Vec::new(),
            };
            result.validate().map_err(Ru41Error::InvalidOutput)?;
            return Ok(result);
        }
        return Err(Ru41Error::InvalidOutput("Pi result must be JSON with summary".into()).into());
    }
    if trimmed.is_empty() {
        return Err(Ru41Error::InvalidOutput("Pi result must be JSON with summary".into()).into());
    }
    Ok(AgentResult {
        summary: trimmed.to_owned(),
        media: Vec::new(),
    })
}

/// 底座最终 JSON 里的媒体列表。
///
/// 两种写法都认：字符串（路径）、对象（kind/source/filename）。
/// kind 缺省按扩展名猜；相对路径由 worker 按运行目录解析。
fn parse_media_list(value: Option<&Value>) -> Vec<MediaRef> {
    let Some(items) = value.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .take(MAX_RESULT_MEDIA)
        .filter_map(|item| {
            let (kind, source, filename) = match item {
                Value::String(s) => (None, s.trim().to_owned(), None),
                Value::Object(map) => {
                    let source = map
                        .get("source")
                        .or_else(|| map.get("path"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_owned())?;
                    let kind = map
                        .get("kind")
                        .and_then(|v| v.as_str())
                        .and_then(parse_media_kind);
                    let filename = map
                        .get("filename")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty());
                    (kind, source, filename)
                }
                _ => return None,
            };
            if source.is_empty() {
                return None;
            }
            let kind = kind.unwrap_or_else(|| guess_media_kind(&source));
            let mut media = MediaRef::new(kind, source);
            media.filename = filename;
            Some(media)
        })
        .collect()
}

/// "image"/"video"/"voice"/"file"（大小写不敏感）。
fn parse_media_kind(raw: &str) -> Option<MediaKind> {
    match raw.to_ascii_lowercase().as_str() {
        "image" => Some(MediaKind::Image),
        "video" => Some(MediaKind::Video),
        "voice" | "audio" => Some(MediaKind::Voice),
        "file" => Some(MediaKind::File),
        _ => None,
    }
}

/// 缺省媒体类型：按扩展名猜。
fn guess_media_kind(source: &str) -> MediaKind {
    let ext = source
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => MediaKind::Image,
        "mp4" | "mov" | "mkv" | "webm" => MediaKind::Video,
        "amr" | "silk" | "wav" | "mp3" | "m4a" => MediaKind::Voice,
        _ => MediaKind::File,
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaKind, is_instruction_deliverable, parse_agent_result, pick_full_report};

    /// 2026-11-23 线上日报的真实形态：MiniMax-M3 把思考段和契约 JSON 放在
    /// **同一个 text block**里，闭标记里夹着 U+200B 零宽字符。
    ///
    /// 之前思考段比真正的日报正文更长 → 契约层「散文更长就以散文为准」的
    /// 兜底把**思考过程**当成交付物推给了用户（11-22、11-23 的日报都这样）。
    #[test]
    fn thinking_prefix_is_stripped_before_delivery() {
        let thinking = "I'll evaluate these candidates against the user's focus areas.\n\n             **Analysis of the candidate list (58 items):**\n\nMost entries are low-signal...\n\n             Write it out.";
        let digest = "# AI / CS 日报 · 2026-11-23\n\n从 58 条候选里挑出 3 条值得看的。";
        let raw = format!("{thinking}</\u{200b}mm:think>{{\"summary\": {digest:?}}}");

        // 契约解析：交付的必须是日报正文，不是思考
        let result = parse_agent_result(&raw).expect("应能解析出契约");
        assert_eq!(result.summary, digest, "交付的必须是正文，不是思考过程");
        assert!(!result.summary.contains("evaluate these candidates"));

        // 裸 JSON（没有契约包裹）也要剥掉思考
        let bare = format!("{thinking}</\u{200b}mm:think>\n{digest}");
        let result = parse_agent_result(&bare).expect("应能解析出正文");
        assert!(
            result.summary.contains("AI / CS 日报"),
            "{}",
            result.summary
        );
        assert!(!result.summary.contains("evaluate these candidates"));
    }

    /// 只有开标记、没有闭标记 → 后面没有正文，返回空（让上层判「没正文」重试），
    /// 好过把思考推给用户。
    #[test]
    fn unterminated_thinking_yields_nothing() {
        let raw = "<think>I'm still thinking... and then some";
        let result = parse_agent_result(raw);
        assert!(result.is_err(), "没正文就不该当成交付物");
    }

    /// 契约里的 summary 又被包了一层 ```json 围栏时，要剥到内层，
    /// 否则投递出去的是原始 JSON 而不是 Markdown（2026-11-18 课堂笔记实测）。
    #[test]
    fn unwraps_a_fenced_contract_inside_the_summary() {
        let inner = "## 本次笔记要点\n\n- 费马引理\n- 罗尔定理";
        // 模型的双层包裹：{"summary": "```json\n{\"summary\": \"## …\"}\n```"}
        let fenced = format!(
            "```json\n{}\n```",
            serde_json::to_string(&serde_json::json!({ "summary": inner })).unwrap()
        );
        let text = serde_json::json!({ "summary": fenced }).to_string();
        let result = parse_agent_result(&text).expect("应能解析出契约");
        assert_eq!(result.summary, inner);
        assert!(!result.summary.contains("```json"), "{}", result.summary);
    }

    /// 报告正文里**引用**的 JSON 代码片段不能被剥掉。
    #[test]
    fn keeps_a_json_snippet_that_is_part_of_a_report() {
        let report =
            "## 配置示例\n\n这段 JSON 要留着：\n\n```json\n{\"a\": 1}\n```\n\n后面还有说明。";
        let text = serde_json::json!({ "summary": report }).to_string();
        let result = parse_agent_result(&text).expect("应能解析出契约");
        assert!(result.summary.contains("\"a\": 1"), "{}", result.summary);
        assert!(
            result.summary.contains("后面还有说明"),
            "{}",
            result.summary
        );
    }

    /// 模型先写完整报告、最后一条 JSON 只给压缩摘要时，交付完整报告。
    #[test]
    fn picks_full_report_over_compressed_summary() {
        let report = format!(
            "# 今日机会雷达（2026-11-06）\n\n## 1. 某某模型发布\n\n- **链接**：https://x\n{}\n",
            "正文细节。".repeat(400)
        );
        let summary = "今日筛选出 7 条值得关注的条目：1) ……；2) ……";
        let picked = pick_full_report(std::slice::from_ref(&report), summary);
        assert_eq!(picked.as_deref(), Some(report.trim()));
    }

    #[test]
    fn strips_contract_envelope_from_picked_report() {
        let body = format!(
            "# 未交作业\n\n## 已过期\n\n- **作业 A** —— 截止 2026-11-08\n{}",
            "补充说明。".repeat(400)
        );
        let report = format!("{body}\n\n```json\n{{\"summary\":\"压缩摘要\"}}\n```");
        let picked = pick_full_report(&[report], "压缩摘要").unwrap();
        assert_eq!(picked, body);
        assert!(!picked.contains("```json"));
        assert!(!picked.contains("压缩摘要"));
    }

    /// 正文不够长、或不像交付物时，不动 summary。
    #[test]
    fn keeps_summary_when_no_full_report() {
        let summary = "今日筛选出 7 条值得关注的条目：1) ……";
        assert!(pick_full_report(&["短消息".to_owned()], summary).is_none());
        let chatter = "我先查一下接口。".repeat(200);
        assert!(pick_full_report(&[chatter], summary).is_none());
    }

    /// 底座最终 JSON 里的媒体：字符串路径与对象两种写法都要认。
    #[test]
    fn parses_media_from_agent_json() {
        let result = parse_agent_result(
            r#"{"summary":"done","media":["shot.png",{"kind":"image","source":"charts/a.png","filename":"a.png"}]}"#,
        )
        .unwrap();
        assert_eq!(result.summary, "done");
        assert_eq!(result.media.len(), 2);
        assert_eq!(result.media[0].source, "shot.png");
        assert_eq!(result.media[0].kind, MediaKind::Image, "缺 kind 按扩展名猜");
        assert_eq!(result.media[1].source, "charts/a.png");
        assert_eq!(result.media[1].filename.as_deref(), Some("a.png"));
    }

    /// media 形状不对时不能连累 summary——整条判失败等于白跑一次。
    #[test]
    fn tolerates_malformed_media() {
        let result = parse_agent_result(
            r#"{"summary":"done","media":[{"nope":1},42,"",{"source":"ok.png"}]}"#,
        )
        .unwrap();
        assert_eq!(result.summary, "done");
        assert_eq!(result.media.len(), 1);
        assert_eq!(result.media[0].source, "ok.png");
    }

    /// 最后一条不是 JSON 时，整段文本当 summary（既有兜底口径）。
    #[test]
    fn plain_text_becomes_summary() {
        let result = parse_agent_result("  直接一段话  ").unwrap();
        assert_eq!(result.summary, "直接一段话");
    }

    /// 模型交了一份没有 summary 的 JSON（论文日报实测：整篇筛选结果被包成
    /// ```ru41-screen 代码块推给了用户）——判契约不符让任务重试，不能原样交付。
    #[test]
    fn rejects_json_without_summary() {
        let raw = "```ru41-screen\n{\"relevant\":[{\"id\":\"2611.09734\",\"title\":\"x\"}]}\n```";
        assert!(
            parse_agent_result(raw).is_err(),
            "无 summary 的 JSON 不该当交付物"
        );
    }

    /// 「机器消费的指令围栏」（ru41-organize）里装的是给插件执行的指令，
    /// 没有 summary 是契约如此——收下它，别判契约不符让任务重试
    /// （2026-11-22 收件箱整理任务重试 9 次后放弃的根因）。
    #[test]
    fn accepts_instruction_fence_as_deliverable() {
        let raw = "```ru41-organize\n{\"moves\":[{\"from\":\"00-Inbox-收件箱/a.md\",\"to\":\"03-Knowledge-知识/a.md\"}],\"deletes\":[],\"todos\":[],\"events\":[],\"skipped\":[]}\n```";
        let result = parse_agent_result(raw).expect("指令围栏该被收下");
        assert!(
            result.summary.contains("ru41-organize"),
            "{}",
            result.summary
        );
        assert!(result.summary.contains("moves"), "{}", result.summary);
    }

    /// 围栏前面带一句过场话也一样认——只取围栏那段，前面的散文不带进去。
    #[test]
    fn accepts_instruction_fence_with_leading_prose() {
        let raw = "整理如下：\n```ru41-organize\n{\"moves\":[],\"skipped\":[]}\n```";
        let result = parse_agent_result(raw).expect("指令围栏该被收下");
        assert!(
            result.summary.starts_with("```ru41-organize"),
            "{}",
            result.summary
        );
        assert!(!result.summary.contains("整理如下"), "{}", result.summary);
    }

    /// 围栏正文不是 JSON → 不算「指令交付物」，按原有兜底当普通文本处理
    /// （插件侧 extract_plan 会拒掉它，走它自己的重试与放弃路径）。
    #[test]
    fn rejects_instruction_fence_with_non_json_body() {
        let raw = "```ru41-organize\n这不是 JSON\n```";
        assert!(
            !is_instruction_deliverable(raw),
            "正文不是 JSON 时不该被当成指令交付物"
        );
    }

    /// 没登记的围栏（论文日报的 ru41-screen）仍然拒收——
    /// 「认指令围栏」不能变成「认所有围栏」。
    #[test]
    fn still_rejects_unregistered_fence() {
        let raw = "```ru41-anything\n{\"relevant\":[]}\n```";
        assert!(parse_agent_result(raw).is_err());
    }

    /// 名单里的每个围栏都要能被认下来——登记了却认不了，等于白登记。
    #[test]
    fn every_registered_fence_is_accepted() {
        for fence in crate::domain::fence::INSTRUCTION_FENCES {
            let raw = format!("```{fence}\n{{\"moves\":[]}}\n```");
            assert!(
                parse_agent_result(&raw).is_ok(),
                "{fence} 登记在名单里，却没被认成交付物"
            );
        }
    }

    /// 散文 + 契约 JSON（模型把 JSON 附在末尾）：交散文，JSON 是信封不外发。
    #[test]
    fn prose_wins_over_trailing_contract_json() {
        let raw = "Vault is clean and the commit landed.\n\n本次笔记要点\n1. OSI 架构\n2. 身份鉴别\n\n{\"summary\":\"## 2026-11-09 课堂笔记全文\"}\n";
        let result = parse_agent_result(raw).unwrap();
        assert!(
            result.summary.contains("本次笔记要点"),
            "{}",
            result.summary
        );
        assert!(
            !result.summary.contains("\"summary\""),
            "{}",
            result.summary
        );
        assert!(
            !result.summary.contains("课堂笔记全文"),
            "{}",
            result.summary
        );
    }

    /// 模型的过场话 + 契约 JSON（2026-11-12 港澳出行问答实测）：散文只是
    /// 「我已拿到所需信息」这种过场话，真答案在契约 JSON 的 summary 里——
    /// 交过场话等于把答案扔了。
    #[test]
    fn preamble_prose_does_not_hide_contract_summary() {
        let preamble = "I have everything needed. Both answers are grounded in the vault travel note \
             (SoSIM procedure) and official SoSIM pages (real-name registration steps), plus \
             Hong Kong's official plug/voltage standards.";
        let answer = format!(
            "# 三角插头能在港直插吗？\n\n{}",
            "不能直插，必须带英标 Type G 转换头。".repeat(40)
        );
        let raw = format!(
            "{preamble}\n\n{{\"summary\":{}}}",
            serde_json::to_string(&answer).unwrap()
        );
        let result = parse_agent_result(&raw).unwrap();
        assert!(result.summary.contains("三角插头"), "{}", result.summary);
        assert!(
            !result.summary.contains("I have everything needed"),
            "{}",
            result.summary
        );
    }

    /// 散文里带括号、契约 JSON 在后面：仍要认出契约对象，不能被散文里的 { 抢走。
    #[test]
    fn contract_json_survives_braces_in_prose() {
        let raw = "说明：集合 {A, B} 之类的记号。\n\n{\"summary\":\"报告正文\"}\n";
        let result = parse_agent_result(raw).unwrap();
        assert_eq!(result.summary, "报告正文");
    }

    /// 只有契约 JSON：交 summary（日报/论文那类就是这种形态）。
    #[test]
    fn contract_json_only_delivers_summary() {
        let result = parse_agent_result("{\"summary\":\"报告正文\"}").unwrap();
        assert_eq!(result.summary, "报告正文");
    }

    /// JSON 之外还有正经正文时取正文，JSON 丢掉。
    #[test]
    fn keeps_prose_next_to_json() {
        let raw = "这是今天筛选的论文报告：\n\n# 论文筛选报告\n\n第一篇很好。\n\n```json\n{\"relevant\":[]}\n```";
        let result = parse_agent_result(raw).unwrap();
        assert!(
            result.summary.contains("论文筛选报告"),
            "{}",
            result.summary
        );
        assert!(
            !result.summary.contains("\"relevant\""),
            "{}",
            result.summary
        );
        assert!(!result.summary.contains("```"), "{}", result.summary);
    }

    /// 带代码围栏的正文 + 契约 JSON：围栏与前置分隔线都只是信封，不能漏给用户。
    #[test]
    fn strips_contract_code_fences_from_prose() {
        let raw = "今天 18:20 下课，最后一节是「五史」教育。\n\n---\n\n```json\n{\"summary\":\"18:20 下课\"}\n```";
        let result = parse_agent_result(raw).unwrap();
        assert_eq!(result.summary, "今天 18:20 下课，最后一节是「五史」教育。");
    }
}
