use anyhow::{Result, bail, ensure};
use ru41::{
    app::{schedule::ScheduleService, todo::TodoService},
    domain::{
        schedule::{EventInput, EventSource, event_time_label, parse_event_when},
        todo::{
            TodoInput, TodoItem, TodoOptions, TodoPriority, TodoSource, TodoStatus, TodoView,
            due_label, parse_due,
        },
    },
    ports::{
        commands::{CommandContext, Reply, SimpleCommand},
        storage::TaskStore,
    },
};
use std::sync::Arc;

/// QQ 私人待办入口；持久化与日期视图由核心 TodoService 提供。
/// `jwxt_semester_start` 来自 jwxt 模块配置，/日程 用它把课表并进同一条时间轴。
pub struct TodoQqCommand {
    options: TodoOptions,
    jwxt_semester_start: Option<chrono::NaiveDate>,
}

/// 待办的唯一录入入口是 `#`：剥掉 `#` 与紧随的分隔符取出正文；
/// 不是捕获语则返回 None。
fn extract_note(content: &str) -> Option<String> {
    let trimmed = content.trim();
    let rest = trimmed.strip_prefix('#')?;
    Some(rest.trim().to_owned())
}

impl TodoQqCommand {
    pub fn new(options: TodoOptions, jwxt_semester_start: Option<chrono::NaiveDate>) -> Self {
        Self {
            options,
            jwxt_semester_start,
        }
    }

    fn service(&self, store: &Arc<dyn TaskStore>) -> TodoService {
        TodoService::new(store.clone(), self.options)
    }

    fn event_service(&self, store: &Arc<dyn TaskStore>) -> ScheduleService {
        ScheduleService::new(store.clone(), self.options)
    }
}

impl SimpleCommand for TodoQqCommand {
    fn verbs(&self) -> &'static [&'static str] {
        &[
            "待办",
            "todo",
            "完成待办",
            "完成",
            "done",
            "推迟待办",
            "推迟",
            "取消待办",
            "恢复待办",
            "restore",
            "日程",
            "行程",
            "schedule",
            "记日程",
            "加日程",
            "schedule-add",
            "取消日程",
        ]
    }

    fn help(&self) -> &'static str {
        "`#内容`：记入待办（开头带时间词自动排期）；单独 `#` 或 `/待办`：全部未完成清单；`/日程`：今日与未来行程"
    }

    fn help_section(&self) -> &'static str {
        "记录与日程"
    }

    async fn run(&self, _arg: &str, _ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        Ok(Some(Reply::text(usage())))
    }

    async fn run_command(
        &self,
        verb: &str,
        arg: &str,
        ctx: &CommandContext<'_>,
    ) -> Result<Option<Reply>> {
        {
            if let Some(reply) = ctx.require_private("使用私人待办", "待办属于个人信息")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let service = self.service(ctx.store);
            let now = ru41::util::now();
            let reply = match verb {
                "待办" | "todo" => self.list_reply(&service, arg, now)?,
                "记日程" | "加日程" | "schedule-add" => {
                    self.event_add_reply(&self.event_service(ctx.store), arg, ctx, now)?
                }
                "日程" | "行程" | "schedule" => {
                    self.event_list_reply(ctx.store, &self.event_service(ctx.store), arg, now)?
                }
                "取消日程" => {
                    ensure!(!arg.is_empty(), "用法：`/取消日程 <日程ID>`");
                    let item = self.event_service(ctx.store).cancel(arg, now)?;
                    Reply::text(format!(
                        "## 已取消日程\n\n- **{}**\n- **ID**：`{}`",
                        item.title,
                        item.short_id()
                    ))
                }
                "完成待办" | "完成" | "done" => {
                    ensure!(!arg.is_empty(), "用法：`/完成 <待办ID>`");
                    let item = service.complete(arg, now)?;
                    // 回执 = 完成确认 + 剩余待办清单：立刻看到划掉一项后的全貌，
                    // 不用再发一次 /待办 确认它还在不在。
                    let remaining: Vec<TodoItem> = service
                        .list(TodoView::All, now, 50)?
                        .into_iter()
                        .filter(|todo| todo.status == TodoStatus::Active)
                        .collect();
                    Reply::text(format!(
                        "## ✅ 已完成：{}\n\n{}",
                        truncate_title(&item.title, 14),
                        format_items("待办", &remaining, now, self.options)
                    ))
                }
                "推迟待办" | "推迟" => {
                    let (selector, until) = split_selector_and_time(arg)
                        .ok_or_else(|| anyhow::anyhow!("用法：`/推迟 <待办ID> 到 <时间>`"))?;
                    let item = service.postpone(selector, until, now)?;
                    Reply::text(format!(
                        "## 已推迟待办\n\n- **{}**\n- **新截止时间**：{}\n- **ID**：`{}`",
                        item.title,
                        due_label(item.due_at, now, self.options, true),
                        item.short_id()
                    ))
                }
                "取消待办" => {
                    ensure!(!arg.is_empty(), "用法：`/取消待办 <待办ID>`");
                    let item = service.cancel(arg, now)?;
                    Reply::text(format!(
                        "## 已取消待办\n\n- **{}**\n- **ID**：`{}`",
                        item.title,
                        item.short_id()
                    ))
                }
                "恢复待办" | "restore" => {
                    ensure!(!arg.is_empty(), "用法：`/恢复待办 <待办ID>`");
                    let item = service.restore(arg, now)?;
                    Reply::text(format!(
                        "## 已恢复待办\n\n- **{}**\n- **ID**：`{}`",
                        item.title,
                        item.short_id()
                    ))
                }
                _ => Reply::text(usage()),
            };
            Ok(Some(reply))
        }
    }

    /// 自然语言捕获：整句以 `#` 或触发前缀开头即视为记录意图。
    /// 单独一个 `#` 相当于 `/待办`：列出全部未完成。
    async fn run_message(&self, content: &str, ctx: &CommandContext<'_>) -> Result<Option<Reply>> {
        let Some(note) = extract_note(content) else {
            return Ok(None);
        };
        if let Some(reply) = ctx.require_private("记录想法", "临时想法属于个人信息")?
        {
            return Ok(Some(Reply::text(reply)));
        }
        let service = self.service(ctx.store);
        let now = ru41::util::now();
        if note.is_empty() {
            return self.list_reply(&service, "", now).map(Some);
        }
        Ok(Some(self.quick_capture_reply(&service, &note, ctx, now)?))
    }
}

/// 私人待办必须保持与水电一致的单聊和身份边界（由 CommandContext::require_private 统一执行）。
impl TodoQqCommand {
    /// 快速捕获：`#内容` 写待办；开头的相对时间词自动识别为截止时间
    ///（`#明天下午三点交报告` → 截止明天 15:00），识别不出时间就进未排期桶。
    /// 不写 Obsidian 文件、也不交给 Agent 二次判断。
    fn quick_capture_reply(
        &self,
        service: &TodoService,
        title: &str,
        ctx: &CommandContext<'_>,
        now: i64,
    ) -> Result<Reply> {
        let title = title.trim();
        ensure!(
            !title.is_empty(),
            "用法：`#内容`（开头可带时间词，如 `#明天下午三点交报告`）"
        );
        let (title, due_at) = match split_leading_time(title, now, &self.options) {
            Some((rest, due)) => (rest, Some(due)),
            None => (title.to_owned(), None),
        };
        let item = service.add(
            TodoInput {
                title,
                priority: TodoPriority::Normal,
                labels: vec!["临时想法".to_owned()],
                due_at,
                remind_at: due_at,
                source: Some(TodoSource {
                    channel: ctx.reply_channel.into(),
                    source_id: Some(ctx.msg_id.to_owned()),
                }),
            },
            now,
        )?;
        // 回执 = 记入确认 + 当前完整待办清单；ID 与查看指令没有信息量，不展示。
        let items: Vec<TodoItem> = service
            .list(TodoView::All, now, 50)?
            .into_iter()
            .filter(|todo| todo.status == TodoStatus::Active)
            .collect();
        Ok(Reply::text(format!(
            "## ✅ 已记入：{}\n\n{}",
            truncate_title(&item.title, 14),
            format_items("待办", &items, now, self.options)
        )))
    }

    fn list_reply(&self, service: &TodoService, arg: &str, now: i64) -> Result<Reply> {
        if arg.trim().is_empty() {
            // 主视图：未完成清单（按截止时间排序）+ 底部近 7 天已完成。
            let items: Vec<TodoItem> = service.list(TodoView::All, now, 50)?;
            return Ok(Reply::text(format_items("待办", &items, now, self.options)));
        }
        let Some(view) = parse_view(arg) else {
            return Ok(Reply::text(format!(
                "## 待办视图不存在\n\n{}",
                usage_views()
            )));
        };
        let items = service.list(view, now, 50)?;
        let title = match view {
            TodoView::Today => "今日待办",
            TodoView::Recent => "近期待办",
            TodoView::LongTerm => "长期待办",
            TodoView::Overdue => "逾期未完成",
            TodoView::Inbox => "未排期",
            TodoView::All => "全部待办",
        };
        Ok(Reply::text(format_items(title, &items, now, self.options)))
    }

    /// `/记日程 <时间> <标题>`：时间在先，支持「明天14:00-16:00」「每周二14:00-15:40」。
    fn event_add_reply(
        &self,
        service: &ScheduleService,
        arg: &str,
        ctx: &CommandContext<'_>,
        now: i64,
    ) -> Result<Reply> {
        let (starts_at, ends_at, recurrence, title) = split_when_and_title(arg, now, self.options)
            .ok_or_else(|| {
                anyhow::anyhow!("用法：`/记日程 <时间> <标题>`，如 `/记日程 明天14:00-16:00 班会`")
            })?;
        let item = service.add(
            EventInput {
                title,
                starts_at,
                ends_at,
                location: None,
                recurrence,
                source: Some(EventSource {
                    channel: ctx.reply_channel.into(),
                    source_id: Some(ctx.msg_id.to_owned()),
                }),
            },
            now,
        )?;
        Ok(Reply::text(format!(
            "## 已添加日程\n\n- **{}**\n- **时间**：{} — {}\n- **ID**：`{}`\n- 查看：`/日程`",
            item.title,
            event_time_label(
                item.starts_at,
                item.recurrence.is_recurring(),
                self.options.offset_seconds
            ),
            event_time_label(item.ends_at, false, self.options.offset_seconds),
            item.short_id()
        )))
    }

    /// `/日程 [今天|明天|N天]`：默认今天 + 未来 7 天；行程之外附带同窗口课表摘要。
    fn event_list_reply(
        &self,
        store: &Arc<dyn TaskStore>,
        service: &ScheduleService,
        arg: &str,
        now: i64,
    ) -> Result<Reply> {
        let days = parse_event_view(arg).unwrap_or(7);
        let occurrences = service.upcoming(now, days, 50)?;
        let mut lines = vec![format!(
            "## 日程（未来 {} 天，行程 {} 项）",
            days,
            occurrences.len()
        )];
        if occurrences.is_empty() {
            lines.push("\n> 暂无行程，用 `/记日程 <时间> <标题>` 添加".to_owned());
        } else {
            for occurrence in &occurrences {
                let mut line = format!(
                    "- {} **{}**",
                    event_time_label(
                        occurrence.starts_at,
                        occurrence.recurring,
                        self.options.offset_seconds
                    ),
                    occurrence.title
                );
                if let Some(location) = &occurrence.location {
                    line.push_str(&format!(" @ {location}"));
                }
                // 文本内「取消日程」✕：点击填入输入框，再点发送即取消。
                let short = occurrence
                    .event_id
                    .strip_prefix("evt_")
                    .unwrap_or(&occurrence.event_id);
                line.push_str(&format!(
                    "　{}",
                    cmd_input_chip("✕", &format!("/取消日程 {short}"))
                ));
                lines.push(line);
            }
        }
        // 同一窗口的课表并入同一条时间轴（来自 jwxt 缓存快照，只读不触网）。
        let today = ru41::domain::schedule::local_date(now, self.options.offset_seconds)
            .unwrap_or_else(|| chrono::Local::now().date_naive());
        let courses = ru41::domain::timetable::courses_summary_from_store(
            store,
            self.jwxt_semester_start,
            today,
            days as u32,
        );
        if !courses.is_empty() {
            lines.push(format!("\n### 📚 课表（{} 天内有课）", courses.len()));
            lines.extend(courses);
        }
        Ok(Reply::text(lines.join("\n")))
    }
}

/// 拼 `<qqbot-cmd-input>` 标签：show 是显示文本，text 是点击后填入输入框的指令；
/// 两个参数按文档要求 urlencode。用于待办复选框与日程列表的行内「✕ 取消」。
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

/// 回执标题截断：最多 max 个字符，超出补省略号。
fn truncate_title(title: &str, max: usize) -> String {
    let mut chars = title.chars();
    let truncated: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// 时间在先：依次尝试前缀 1..=3 个空白分隔词作为时间文本，命中后其余为标题。
fn split_when_and_title(
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

fn parse_event_view(arg: &str) -> Option<i64> {
    match arg.trim() {
        "" | "今天" | "今日" => Some(1),
        "明天" | "明日" => Some(2),
        text => text
            .strip_suffix('天')
            .and_then(|n| n.parse::<i64>().ok())
            .filter(|n| (1..=30).contains(n)),
    }
}

fn parse_view(text: &str) -> Option<TodoView> {
    match text.trim().to_lowercase().as_str() {
        "今日" | "今天" | "today" => Some(TodoView::Today),
        "近期" | "最近" | "recent" => Some(TodoView::Recent),
        "长期" | "long" | "long-term" => Some(TodoView::LongTerm),
        "逾期" | "过期" | "overdue" => Some(TodoView::Overdue),
        "收件箱" | "未排期" | "inbox" => Some(TodoView::Inbox),
        "全部" | "所有" | "all" => Some(TodoView::All),
        _ => None,
    }
}

/// 从快速捕获文本里提取开头的相对时间词：「明天下午三点交报告」→ ("交报告", due)。
/// 只认 parse_due 支持的形态（今天/明天/明晚/后天/今晚/N天后 + 上午/下午/晚上/晚 +
/// 钟点），且时间词后面必须还有标题内容，否则整句按标题处理、不排期。
fn split_leading_time(text: &str, now: i64, options: &TodoOptions) -> Option<(String, i64)> {
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
fn leading_time_len(text: &str) -> Option<usize> {
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

fn split_selector_and_time(arg: &str) -> Option<(&str, &str)> {
    let (selector, mut until) = arg.split_once(char::is_whitespace)?;
    until = until.trim();
    if let Some(rest) = until.strip_prefix('到') {
        until = rest.trim();
    }
    (!selector.is_empty() && !until.is_empty()).then_some((selector, until))
}

fn format_items(title: &str, items: &[TodoItem], now: i64, options: TodoOptions) -> String {
    let active: Vec<&TodoItem> = items
        .iter()
        .filter(|item| item.status == TodoStatus::Active)
        .collect();
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
        lines.push(String::new());
        lines.push(format!("### 🏁 已完成（近 7 天，{}）", completed.len()));
        lines.extend(completed.iter().map(|item| format_completed(item)));
    }
    lines.join("\n")
}

/// 已完成条目：**🗹** 粗勾 + 标题划横线。
fn format_completed(item: &TodoItem) -> String {
    format!("**🗹** ~~{}~~", item.title)
}

/// 近 7 天的已完成：完成时间在窗口内，或创建时间在窗口内（近七天创建的待办也留着）。
fn is_recent_completed(item: &TodoItem, now: i64) -> bool {
    const WEEK: i64 = 7 * 86_400;
    item.completed_at.is_some_and(|at| at >= now - WEEK) || item.created_at >= now - WEEK
}

fn format_item(item: &TodoItem, now: i64, options: TodoOptions) -> String {
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
    format!("{checkbox}**{}**{due}{priority}", item.title)
}

/// 待办「完成」复选框：`<qqbot-cmd-input show="囗">`，点击后指令填入输入框、
/// 再点发送即完成（两跳是官方能力的代价：cmd-enter 一跳但不支持自定义显示——
/// 平台报错 40034108；mqqapi 链接能显示但新版客户端点击无响应）。
/// 符号选型（2026-11-09）：☐（U+2610）文本态太小、⬜️（U+2B1C）是无边框白块；
/// 囗（U+56D7，CJK 表意文字）按汉字全角尺寸渲染——大、有边框、空心，像复选框。
fn done_chip(item: &TodoItem) -> String {
    cmd_input_chip("囗", &format!("/完成 {}", item.short_id()))
}

fn usage() -> String {
    format!(
        "## 待办用法\n\n- `#内容`：记入待办（开头带时间词自动排期，如 `#明天下午三点交报告`）\n- `#` 或 `/待办`：全部未完成（复选框列表，点框后发送即完成）\n- `/待办 <{}>`\n- `/完成 <待办ID>`\n- `/推迟 <待办ID> 到 <时间>`\n- `/取消待办 <待办ID>`\n\n## 日程用法\n\n- `/日程 [今天|明天|N天]`：今日起行程\n- `/记日程 <时间> <标题>`：如 `明天14:00-16:00 班会`、`每周二14:00-15:40 体育课`\n- `/取消日程 <日程ID>`",
        usage_views()
    )
}

fn usage_views() -> String {
    "今日|近期|长期|逾期|未排期|全部".to_owned()
}

#[cfg(test)]
mod tests {
    use super::{cmd_input_chip, format_items, split_leading_time, truncate_title, url_encode};

    #[test]
    fn split_leading_time_extracts_compact_relative_time() {
        let options = ru41::domain::todo::TodoOptions {
            offset_seconds: 8 * 3600,
            ..Default::default()
        };
        let now = 1_794_152_881; // 2026-11-08 傍晚（UTC+8）
        // 紧凑写法：日期词 + 时段 + 钟点。
        let (title, due) = split_leading_time("明天下午三点交实验报告", now, &options).unwrap();
        assert_eq!(title, "交实验报告");
        let offset = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
        let local = chrono::DateTime::from_timestamp(due, 0)
            .unwrap()
            .with_timezone(&offset);
        assert_eq!(local.format("%H:%M").to_string(), "15:00");
        // 只有日期词也行（默认 22:00）。
        let (title, _) = split_leading_time("明天交作业", now, &options).unwrap();
        assert_eq!(title, "交作业");
        // 时段+钟点（无日期词）= 今天。
        assert!(split_leading_time("下午三点开会", now, &options).is_some());
        // 只有时段没有钟点：不识别（不随便安默认时间）。
        assert!(split_leading_time("下午交作业", now, &options).is_none());
        // 裸数字不算钟点：「明天3号」不吃成凌晨三点。
        assert!(
            split_leading_time("明天3号交报告", now, &options).is_none_or(|(_, due)| {
                let local = chrono::DateTime::from_timestamp(due, 0)
                    .unwrap()
                    .with_timezone(&offset);
                local.format("%H:%M").to_string() == "22:00"
            })
        );
        // 没有时间词：不识别。
        assert!(split_leading_time("板子焊接", now, &options).is_none());
        // 整句都是时间词：不识别（没有标题内容）。
        assert!(split_leading_time("明天下午三点", now, &options).is_none());
    }

    #[test]
    fn url_encode_escapes_non_unreserved() {
        assert_eq!(url_encode("/完成 abc123"), "%2F%E5%AE%8C%E6%88%90%20abc123");
        // 字母数字与 unreserved 字符不编码，「/」与空格编码。
        assert_eq!(url_encode("/done a1-b2_c3.4~"), "%2Fdone%20a1-b2_c3.4~");
    }

    #[test]
    fn cmd_input_chip_encodes_show_and_text() {
        assert_eq!(
            cmd_input_chip("☐", "/完成 abc123"),
            "<qqbot-cmd-input text=\"%2F%E5%AE%8C%E6%88%90%20abc123\" \
             show=\"%E2%98%90\" reference=\"false\" />"
        );
    }

    #[test]
    fn renders_active_list_and_recent_completed_footer() {
        use ru41::domain::todo::{TodoItem, TodoOptions, TodoStatus};
        let options = TodoOptions::default();
        let now = 1_794_152_881;
        let mut active = TodoItem {
            id: "todo_1".into(),
            title: "板子焊接".into(),
            status: TodoStatus::Active,
            priority: ru41::domain::todo::TodoPriority::Normal,
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
        let text = format_items("待办", &[active.clone(), done], now, options);
        // 未完成在前，计数只算未完成。
        assert!(text.contains("## 待办（1）"), "{text}");
        // 复选框后没有空格。
        assert!(text.contains("**板子焊接**"), "{text}");
        assert!(!text.contains("　**板子焊接**"), "{text}");
        // 底部已完成区：粗勾 + 划横线。
        assert!(text.contains("### 🏁 已完成（近 7 天，1）"), "{text}");
        assert!(text.contains("**🗹** ~~写周报~~"), "{text}");
        active.completed_at = None;
    }

    #[test]
    fn truncate_title_caps_at_max_chars() {
        assert_eq!(truncate_title("短标题", 14), "短标题");
        let long = "这是一段特别特别长的待办标题内容";
        let truncated = truncate_title(long, 14);
        assert_eq!(truncated.chars().count(), 15); // 14 字 + 省略号
        assert!(truncated.ends_with('…'));
        // 恰好 14 字不补省略号。
        assert_eq!(
            truncate_title("一二三四五六七八九十一二三四", 14),
            "一二三四五六七八九十一二三四"
        );
    }
}
