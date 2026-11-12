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
            return Some((candidate, summary.to_owned()));
        }
        offset = end + 1;
    }
    None
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

pub(super) fn parse_agent_result(text: &str) -> Result<AgentResult> {
    let trimmed = text.trim();
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
    use super::{MediaKind, parse_agent_result, pick_full_report};

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
