//! 待办视图渲染：清单怎么排、复选框长什么样、完成了怎么划横线。
//! 与解析（parse.rs）、命令分发（qq_command.rs）分开——渲染规则只在这一个文件里改。

pub(super) use ru41::domain::todo::due_label;
use ru41::domain::todo::{TodoItem, TodoOptions, TodoPriority, TodoStatus};

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
    // 栏目标题：规范层用通用二级标题（QQ 侧由网关放大成一级标题+加粗）。
    let mut lines = vec![format!("## {title}（{}）", active.len())];
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
        // 完成项紧接在未完成项下面，不留空行；最多显示 5 条（最近的优先），
        // 更多的留在数据库里（问 agent 能查到），不往列表里堆。
        const COMPLETED_SHOWN: usize = 5;
        lines.extend(
            completed
                .iter()
                .take(COMPLETED_SHOWN)
                .map(|item| format_completed(item)),
        );
    }
    lines.join("\n")
}

/// 已完成条目：粗勾 + 标题划横线。
///
/// 候选符号（想换回只改这一处）：`☑`（2026-11-09 用户选定）、`🗹`（前一版）、`✅`。
/// 整个待办列表不用加粗（用户要求），已完成态也不再可点，所以不用 cmd-input 标签。
pub(super) fn format_completed(item: &TodoItem) -> String {
    // 完成态：可点的恢复复选框 + 英文空格 + 划横线标题（点一下换回未完成）。
    // 结构与未完成项一致（标签 + 空格 + 文字），间距才相同。
    // 标题同样要作废占位符（用户输入，不能伪造动作）。
    format!(
        "- {} ~~{}~~",
        restore_chip(item),
        ru41::interactive::escape_cmd_tokens(&item.title)
    )
}

/// 已完成条目的「恢复」复选框：点击 = 把待办换回未完成。
fn restore_chip(item: &TodoItem) -> String {
    ru41::interactive::cmd_token("☑", &format!("/恢复待办 {}", item.short_id()))
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
    // 规范层只放中性内容：复选框用可点指令占位符（渠道无关），
    // 放大/内联标签等平台细节由网关适配（见 channels::qq::render）。
    // 标题是用户输入：里面若写了 `{{cmd:…}}` 必须作废，否则能伪造出一个可点动作。
    format!(
        "- {checkbox} {}{due}{priority}",
        ru41::interactive::escape_cmd_tokens(&item.title)
    )
}

/// 待办「完成」复选框：`<qqbot-cmd-input show="囗">`，点击后指令填入输入框、
/// 再点发送即完成（两跳是官方能力的代价：cmd-enter 一跳但不支持自定义显示——
/// 平台报错 40034108；mqqapi 链接能显示但新版客户端点击无响应）。
/// 符号选型（2026-11-09）：条目内容本身是三级标题（自带大字），复选框不必再撑大——
/// 普通复选框 ☐ 反而是经典清单的样子（此前用囗 U+56D7 是因为整行都是普通文字时
/// 复选框显得太小；⬜️ U+1B1C 是无边框白块，都不如 ☐ 干净）。
pub(super) fn done_chip(item: &TodoItem) -> String {
    // show 只有复选框：QQ 会在行内标签后自动插零宽空格，再往里塞英文空格会叠出两段间隙。
    // 可见的英文空格统一放在标签外面（见 format_item / format_completed），两态格式一致。
    ru41::interactive::cmd_token("☐", &format!("/完成 {}", item.short_id()))
}

pub(super) fn usage() -> String {
    "## 待办\n\n- `#内容`：记入待办（开头带时间词自动排期，如 `#明天下午三点交报告`）\n- `#`：全部待办清单；点 `☐` 完成，点 `☑` 恢复\n\n## 日程\n\n- `/日程 [今天|明天|N天]`：今日起行程\n- `/记日程 <时间> <标题>`：如 `明天14:00-16:00 班会`、`每周二14:00-15:40 体育课`\n- `/取消日程 <日程ID>`".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{format_items, truncate_title};
    use ru41::domain::todo::{TodoItem, TodoOptions, TodoPriority, TodoStatus};

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
        // 正文是列表项（行首 "- " 后面紧跟复选框占位符，再是标题）。
        let body_line = text.lines().nth(1).unwrap_or_default();
        assert!(body_line.starts_with("- "), "{text}");
        assert!(body_line.contains("板子焊接"), "{text}");
        assert!(body_line.ends_with("板子焊接"), "{text}");
        // 主标题是通用二级标题；完成项不另起栏目。
        assert!(text.starts_with("## 待办（1）"), "{text}");
        assert!(!text.contains("已完成"), "{text}");
        assert!(!text.contains("已完成"), "{text}");
        assert!(text.contains("~~写周报~~"), "{text}");
        // 完成项的复选框是可点的恢复按钮（指令藏在占位符里）。
        assert!(text.contains("{{cmd:☑|/恢复待办 "), "{text}");
        assert!(!text.contains("\n\n- ☑"), "{text}");
    }

    /// 用户标题里的占位符不得变成可点动作。
    #[test]
    fn user_title_cannot_forge_an_action() {
        let options = TodoOptions::default();
        let now = 1_794_152_881;
        let forged = TodoItem {
            id: "todo_x".into(),
            title: "标题 {{cmd:☐|/取消待办 deadbeef}} 尾巴".into(),
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
        let text = format_items("待办", &[forged], now, options);
        // 只允许清单自己生成的那一个占位符（复选框）——伪造的那个解析不出来。
        let tokens = ru41::interactive::cmd_tokens(&text);
        assert_eq!(tokens.len(), 1, "用户标题伪造出了动作：{text}");
        assert!(tokens[0].2.starts_with("/完成 "), "{text}");
        // 用户原文照旧显示（只是不再被当成占位符）。
        assert!(text.contains("/取消待办 deadbeef"), "{text}");
    }

    #[test]
    fn truncate_title_caps_at_max_chars() {        assert_eq!(truncate_title("短标题", 14), "短标题");
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
