//! 待办视图渲染：清单怎么排、复选框长什么样、完成了怎么划横线。
//! 与解析（parse.rs）、命令分发（qq_command.rs）分开——渲染规则只在这一个文件里改。

pub(super) use ru41::domain::todo::due_label;
use ru41::domain::todo::{TodoItem, TodoOptions, TodoPriority, TodoStatus};

/// 拼 `<qqbot-cmd-input>` 标签：show 是显示文本，text 是点击后填入输入框的指令；
/// 两个参数按文档要求 urlencode。用于待办复选框与日程列表的行内「✕ 取消」。
pub(super) fn cmd_input_chip(show: &str, command: &str) -> String {
    format!(
        "<qqbot-cmd-input text=\"{}\" show=\"{}\" reference=\"false\" />",
        url_encode(command),
        url_encode(show)
    )
}

/// percent-encode：保留 RFC 3986 unreserved，其余按 UTF-8 字节转义。
pub(super) fn url_encode(text: &str) -> String {
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

/// 回执标题截断：最多 max 个字符，超出补省略号。
pub(super) fn truncate_title(title: &str, max: usize) -> String {
    let mut chars = title.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

pub(super) fn format_items(
    title: &str,
    items: &[TodoItem],
    now: i64,
    options: TodoOptions,
) -> String {
    let active: Vec<&TodoItem> = items
        .iter()
        .filter(|item| item.status == TodoStatus::Active)
        .collect();
    // 主标题保留 markdown 标题格式（`## 待办（3）`）；下面的完成项不另起栏目。
    // 栏目标题：一级标题（最大号）+ 显式加粗（移动端渲染双保险）。
    let mut lines = vec![format!("# **{title}（{}）**", active.len())];
    if active.is_empty() {
        lines.push("\n> 暂无".to_owned());
    } else {
        lines.extend(active.iter().map(|item| format_item(item, now, options)));
    }
    // 底部：近 7 天的已完成（粗勾 + 划横线）——一眼看到「干掉了多少」。
    let mut completed: Vec<&TodoItem> = items
        .iter()
        .filter(|item| item.status != TodoStatus::Active && is_recent_completed(item, now))
        .collect();
    // 最近完成的排最前。
    completed.sort_by(|a, b| {
        b.completed_at
            .unwrap_or(b.created_at)
            .cmp(&a.completed_at.unwrap_or(a.created_at))
    });
    if !completed.is_empty() {
        // 完成项紧接在未完成项下面，不留空行。
        lines.extend(completed.iter().map(|item| format_completed(item)));
    }
    lines.join("\n")
}

/// 已完成条目：粗勾 + 标题划横线。
///
/// 候选符号（想换回只改这一处）：`☑`（2026-11-09 用户选定）、`🗹`（前一版）、`✅`。
/// 整个待办列表不用加粗（用户要求），已完成态也不再可点，所以不用 cmd-input 标签。
pub(super) fn format_completed(item: &TodoItem) -> String {
    // 完成态同样用三级标题 + 划横线。
    format!("### ☑ ~~{}~~", item.title)
}

/// 近 7 天的已完成：判定在核心里（domain::todo::is_recent_completed），
/// QQ 视图与提醒共用一份规则——按创建日期算，只控制显示，不删数据。
pub(super) fn is_recent_completed(item: &TodoItem, now: i64) -> bool {
    ru41::domain::todo::is_recent_completed(item, now, 7)
}

pub(super) fn format_item(item: &TodoItem, now: i64, options: TodoOptions) -> String {
    let priority = match item.priority {
        TodoPriority::High => "，高优先级",
        TodoPriority::Low => "，低优先级",
        TodoPriority::Normal => "",
    };
    // 行首大复选框：活跃条目是「完成」（囗）。复选框后不加空格，
    // 标题紧随其后（加粗复选框 QQ 客户端不渲染，试过，不管用）。
    let checkbox = done_chip(item);
    // 未排期（无截止时间）不显示时间尾巴——「未排期」三个字没有信息量。
    let due = if item.due_at.is_some() {
        format!(
            " — {}",
            due_label(item.due_at, now, options, item.status == TodoStatus::Active)
        )
    } else {
        String::new()
    };
    // 正文用三级标题：QQ markdown 没有字号语法，只有标题层级能放大。
    // 层级：## 栏目标题 > ### 正文条目。
    format!("### {checkbox}{}{due}{priority}", item.title)
}

/// 待办「完成」复选框：`<qqbot-cmd-input show="囗">`，点击后指令填入输入框、
/// 再点发送即完成（两跳是官方能力的代价：cmd-enter 一跳但不支持自定义显示——
/// 平台报错 40034108；mqqapi 链接能显示但新版客户端点击无响应）。
/// 符号选型（2026-11-09）：☐（U+2610）文本态太小、⬜️（U+2B1C）是无边框白块；
/// 囗（U+56D7，CJK 表意文字）按汉字全角尺寸渲染——大、有边框、空心，像复选框。
pub(super) fn done_chip(item: &TodoItem) -> String {
    cmd_input_chip("囗", &format!("/完成 {}", item.short_id()))
}

pub(super) fn usage() -> String {
    "## 待办用法\n\n- `#内容`：记入待办（开头带时间词自动排期，如 `#明天下午三点交报告`）\n- `#`：全部待办清单（复选框列表，点框后发送即完成；完成的划横线排在下面）\n- `/完成 <待办ID>`\n- `/推迟 <待办ID> 到 <时间>`\n- `/取消待办 <待办ID>`\n\n## 日程用法\n\n- `/日程 [今天|明天|N天]`：今日起行程\n- `/记日程 <时间> <标题>`：如 `明天14:00-16:00 班会`、`每周二14:00-15:40 体育课`\n- `/取消日程 <日程ID>`".to_owned()
}

pub(super) fn usage_views() -> String {
    "今日|近期|长期|逾期|未排期|全部".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{cmd_input_chip, format_items, truncate_title, url_encode};
    use ru41::domain::todo::{TodoItem, TodoOptions, TodoPriority, TodoStatus};

    #[test]
    fn url_encode_escapes_non_unreserved() {
        assert_eq!(url_encode("/完成 abc123"), "%2F%E5%AE%8C%E6%88%90%20abc123");
        assert_eq!(url_encode("/done a1-b2_c3.4~"), "%2Fdone%20a1-b2_c3.4~");
    }

    #[test]
    fn cmd_input_chip_encodes_show_and_text() {
        assert_eq!(
            cmd_input_chip("囗", "/完成 abc123"),
            "<qqbot-cmd-input text=\"%2F%E5%AE%8C%E6%88%90%20abc123\" \
             show=\"%E5%9B%97\" reference=\"false\" />"
        );
    }

    #[test]
    fn renders_active_list_and_recent_completed_footer() {
        let options = TodoOptions::default();
        let now = 1_794_152_881;
        let active = TodoItem {
            id: "todo_1".into(),
            title: "板子焊接".into(),
            status: TodoStatus::Active,
            priority: TodoPriority::Normal,
            labels: vec![],
            due_at: None,
            remind_at: None,
            last_reminded_at: None,
            completed_at: None,
            created_at: now,
            updated_at: now,
            source: None,
        };
        let mut done = active.clone();
        done.id = "todo_2".into();
        done.title = "写周报".into();
        done.status = TodoStatus::Completed;
        done.completed_at = Some(now - 3600);
        done.created_at = now - 7200;
        let text = format_items("待办", &[active, done], now, options);
        assert!(text.contains("待办（1）"), "{text}");
        // 正文是三级标题行（行首 ### 后面紧跟复选框标签，再是标题）。
        let body_line = text.lines().nth(1).unwrap_or_default();
        assert!(body_line.starts_with("### "), "{text}");
        assert!(body_line.ends_with("板子焊接"), "{text}");
        // 主标题是 markdown 标题；完成项不另起栏目。
        assert!(text.starts_with("# **待办（1）**"), "{text}");
        assert!(!text.contains("已完成"), "{text}");
        assert!(!text.contains("已完成"), "{text}");
        assert!(text.contains("### ☑ ~~写周报~~"), "{text}");
        assert!(!text.contains("\n\n### ☑"), "{text}");
    }

    #[test]
    fn truncate_title_caps_at_max_chars() {
        assert_eq!(truncate_title("短标题", 14), "短标题");
        let long = "这是一段特别特别长的待办标题内容";
        let truncated = truncate_title(long, 14);
        assert_eq!(truncated.chars().count(), 15);
        assert!(truncated.ends_with('…'));
        assert_eq!(
            truncate_title("一二三四五六七八九十一二三四", 14),
            "一二三四五六七八九十一二三四"
        );
    }
}
