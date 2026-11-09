//! 自然语言解析：从「帮我记住 ftp 的密码是 xxx」「明天下午三点」这类输入里
//! 抽出结构化字段。只解析，不落库、不渲染。

use ru41::domain::{
    schedule::parse_event_when,
    todo::{TodoOptions, parse_due},
};

/// 待办的唯一录入入口是 `#`：剥掉 `#` 与紧随的分隔符取出正文；
/// 不是捕获语则返回 None。
pub(super) fn extract_note(content: &str) -> Option<String> {
    let trimmed = content.trim();
    let rest = trimmed.strip_prefix('#')?;
    Some(rest.trim().to_owned())
}

/// 时间在先：依次尝试前缀 1..=3 个空白分隔词作为时间文本，命中后其余为标题。
pub(super) fn split_when_and_title(
    arg: &str,
    now: i64,
    options: TodoOptions,
) -> Option<(i64, i64, ru41::domain::schedule::EventRecurrence, String)> {
    let parts: Vec<&str> = arg.split_whitespace().collect();
    for end in 1..=parts.len().min(3) {
        let candidate = parts[..end].join(" ");
        if let Some((starts_at, ends_at, recurrence)) = parse_event_when(&candidate, now, &options)
        {
            let title = parts[end..].join(" ");
            if !title.trim().is_empty() {
                return Some((starts_at, ends_at, recurrence, title));
            }
        }
    }
    None
}

pub(super) fn parse_event_view(arg: &str) -> Option<i64> {
    match arg.trim() {
        "" | "今天" | "今日" => Some(1),
        "明天" | "明日" => Some(2),
        text => text
            .strip_suffix('天')
            .and_then(|n| n.parse::<i64>().ok())
            .filter(|n| (1..=30).contains(n)),
    }
}

/// 从快速捕获文本里提取开头的相对时间词：「明天下午三点交报告」→ ("交报告", due)。
/// 只认 parse_due 支持的形态（今天/明天/明晚/后天/今晚/N天后 + 上午/下午/晚上/晚 +
/// 钟点），且时间词后面必须还有标题内容，否则整句按标题处理、不排期。
pub(super) fn split_leading_time(
    text: &str,
    now: i64,
    options: &TodoOptions,
) -> Option<(String, i64)> {
    let end = leading_time_len(text)?;
    let (when, rest) = text.split_at(end);
    let rest = rest.trim_start();
    if rest.is_empty() {
        return None;
    }
    let due = parse_due(when, now, options).ok()?;
    Some((rest.to_owned(), due))
}

/// 扫描开头时间词的字节边界：日期词 + 时段词 + 钟点。
/// 有日期词时，时段/钟点可有可无；没有日期词则时段+钟点必须同时出现
///（避免「#下午交作业」被安一个默认 22:00 的今晚截止）。
/// 裸数字不算钟点（要带 点/时/: 后缀），防止「明天3号」被吃成凌晨 3 点。
pub(super) fn leading_time_len(text: &str) -> Option<usize> {
    let mut rest = text;
    let mut matched_day = false;
    for word in ["今天", "今日", "明天", "明晚", "后天", "今晚"] {
        if let Some(r) = rest.strip_prefix(word) {
            rest = r;
            matched_day = true;
            break;
        }
    }
    if !matched_day {
        let digits: usize = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0
            && let Some(r) = rest[digits..].strip_prefix("天后")
        {
            rest = r;
            matched_day = true;
        }
    }
    let mut matched_period = false;
    for word in ["上午", "下午", "晚上", "晚"] {
        if let Some(r) = rest.strip_prefix(word) {
            rest = r;
            matched_period = true;
            break;
        }
    }
    let mut matched_clock = false;
    // 钟点字符类：阿拉伯数字与中文数字字 + 分隔符。标题以数字字开头时
    // 会被多吃，所以从长到短回退到 parse_clock 能接受的最长前缀。
    let clock_len: usize = rest
        .chars()
        .take_while(|c| {
            c.is_ascii_digit()
                || matches!(
                    c,
                    '零' | '一'
                        | '二'
                        | '两'
                        | '三'
                        | '四'
                        | '五'
                        | '六'
                        | '七'
                        | '八'
                        | '九'
                        | '十'
                        | '点'
                        | '时'
                        | '分'
                        | '半'
                        | ':'
                )
        })
        .map(char::len_utf8)
        .sum();
    if clock_len > 0 {
        let mut end = clock_len;
        while end > 0 {
            let candidate = &rest[..end];
            // 必须带点/时/: 后缀结构：裸数字（明天3号）不算钟点。
            let structured = candidate.contains(['点', '时', ':']);
            if structured && ru41::domain::todo::parse_clock(candidate).is_some() {
                rest = &rest[end..];
                matched_clock = true;
                break;
            }
            end -= candidate.chars().last().map(char::len_utf8).unwrap_or(1);
        }
    }
    if matched_day || (matched_period && matched_clock) {
        Some(text.len() - rest.len())
    } else {
        None
    }
}

pub(super) fn split_selector_and_time(arg: &str) -> Option<(&str, &str)> {
    let (selector, mut until) = arg.split_once(char::is_whitespace)?;
    until = until.trim();
    if let Some(rest) = until.strip_prefix('到') {
        until = rest.trim();
    }
    (!selector.is_empty() && !until.is_empty()).then_some((selector, until))
}

#[cfg(test)]
mod tests {
    use super::split_leading_time;
    use ru41::domain::todo::TodoOptions;

    #[test]
    fn split_leading_time_extracts_compact_relative_time() {
        let options = TodoOptions {
            offset_seconds: 8 * 3600,
            ..Default::default()
        };
        let now = 1_794_152_881;
        let (title, due) = split_leading_time("明天下午三点交实验报告", now, &options).unwrap();
        assert_eq!(title, "交实验报告");
        let offset = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let local = chrono::DateTime::from_timestamp(due, 0)
            .unwrap()
            .with_timezone(&offset);
        assert_eq!(local.format("%H:%M").to_string(), "15:00");
        let (title, _) = split_leading_time("明天交作业", now, &options).unwrap();
        assert_eq!(title, "交作业");
        assert!(split_leading_time("下午三点开会", now, &options).is_some());
        assert!(split_leading_time("下午交作业", now, &options).is_none());
        assert!(split_leading_time("板子焊接", now, &options).is_none());
        assert!(split_leading_time("明天下午三点", now, &options).is_none());
    }
}
