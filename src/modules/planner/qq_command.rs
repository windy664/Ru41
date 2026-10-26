use crate::{
    app::{schedule::ScheduleService, todo::TodoService},
    domain::{
        schedule::{EventInput, EventSource, event_time_label, parse_event_when},
        todo::{
            TodoInput, TodoItem, TodoOptions, TodoPriority, TodoSource, TodoStatus, TodoView,
            due_label, parse_due,
        },
    },
    ports::{
        commands::{CommandContext, CommandHandler, MarkupButton, Reply, ReplyMarkup},
        storage::TaskStore,
    },
};
use anyhow::{Result, bail, ensure};
use std::{future::Future, pin::Pin, sync::Arc};

/// QQ 私人待办入口；持久化与日期视图由核心 TodoService 提供。
pub struct TodoQqCommand {
    options: TodoOptions,
}

impl TodoQqCommand {
    pub fn new(options: TodoOptions) -> Self {
        Self { options }
    }

    fn service(&self, store: &Arc<dyn TaskStore>) -> TodoService {
        TodoService::new(store.clone(), self.options)
    }

    fn event_service(&self, store: &Arc<dyn TaskStore>) -> ScheduleService {
        ScheduleService::new(store.clone(), self.options)
    }
}

impl CommandHandler for TodoQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(
            verb,
            "待办"
                | "todo"
                | "记待办"
                | "新增待办"
                | "todo-add"
                | "完成待办"
                | "完成"
                | "done"
                | "推迟待办"
                | "推迟"
                | "取消待办"
                | "恢复待办"
                | "restore"
                | "日程"
                | "行程"
                | "schedule"
                | "记日程"
                | "加日程"
                | "schedule-add"
                | "取消日程"
        )
    }

    fn help(&self) -> &'static str {
        "`/待办`：今日、逾期、长期事项；`/记待办 [时间] <内容>` 新增；`/日程`：今日与未来行程；`/记日程 <时间> <标题>` 新增"
    }

    fn handle<'a>(
        &'a self,
        _arg: &'a str,
        _ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async { Ok(Some(Reply::text(usage()))) })
    }

    fn handle_command<'a>(
        &'a self,
        verb: &'a str,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(reply) = ctx.require_private("使用私人待办", "待办属于个人信息")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            let service = self.service(ctx.store);
            let now = crate::util::now();
            let reply = match verb {
                "待办" | "todo" => {
                    self.list_reply(&service, &self.event_service(ctx.store), arg, now)?
                }
                "记日程" | "加日程" | "schedule-add" => {
                    self.event_add_reply(&self.event_service(ctx.store), arg, ctx, now)?
                }
                "日程" | "行程" | "schedule" => {
                    self.event_list_reply(&self.event_service(ctx.store), arg, now)?
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
                "记待办" | "新增待办" | "todo-add" => {
                    self.add_reply(&service, arg, ctx, now)?
                }
                "完成待办" | "完成" | "done" => {
                    ensure!(!arg.is_empty(), "用法：`/完成 <待办ID>`");
                    let item = service.complete(arg, now)?;
                    Reply::text(format!(
                        "## 已完成待办\n\n- **{}**\n- **ID**：`{}`",
                        item.title,
                        item.short_id()
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
        })
    }
}

/// 私人待办必须保持与水电一致的单聊和身份边界（由 CommandContext::require_private 统一执行）。
impl TodoQqCommand {
    fn list_reply(
        &self,
        service: &TodoService,
        events: &ScheduleService,
        arg: &str,
        now: i64,
    ) -> Result<Reply> {
        if arg.trim().is_empty() {
            let overview = service.overview(now, 20)?;
            let today_events = events.today(now)?;
            let markup = done_markup(&overview.today);
            return Ok(Reply::with_markup(
                format_overview(&overview, &today_events, now, self.options),
                markup,
            ));
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
            TodoView::Inbox => "收件箱",
            TodoView::All => "全部待办",
        };
        Ok(Reply::with_markup(
            format_items(title, &items, now, self.options),
            done_markup(&items),
        ))
    }

    fn add_reply(
        &self,
        service: &TodoService,
        arg: &str,
        ctx: &CommandContext<'_>,
        now: i64,
    ) -> Result<Reply> {
        let (title, due_at) = split_title_and_due(arg, now, self.options)?;
        ensure!(!title.is_empty(), "用法：`/记待办 [时间] <内容>`");
        let item = service.add(
            TodoInput {
                title,
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at,
                remind_at: due_at,
                source: Some(TodoSource {
                    channel: ctx.reply_channel.into(),
                    source_id: Some(ctx.msg_id.to_owned()),
                }),
            },
            now,
        )?;
        Ok(Reply::text(format!(
            "## 已添加待办\n\n- **{}**\n- **截止**：{}\n- **ID**：`{}`\n- 查看：`/待办`",
            item.title,
            due_label(item.due_at, now, self.options, true),
            item.short_id()
        )))
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

    /// `/日程 [今天|明天|N天]`：默认今天 + 未来 7 天。
    fn event_list_reply(&self, service: &ScheduleService, arg: &str, now: i64) -> Result<Reply> {
        let days = parse_event_view(arg).unwrap_or(7);
        let occurrences = service.upcoming(now, days, 50)?;
        let mut lines = vec![format!(
            "## 行程（未来 {} 天，{} 项）",
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
                lines.push(line);
            }
        }
        let mut markup = ReplyMarkup::new();
        for occurrence in occurrences.iter().take(4) {
            let short = occurrence
                .event_id
                .strip_prefix("evt_")
                .unwrap_or(&occurrence.event_id);
            markup.push_row(vec![MarkupButton::new(
                format!("取消日程 {}", occurrence.title),
                format!("/取消日程 {short}"),
            )]);
        }
        Ok(Reply::with_markup(lines.join("\n"), markup))
    }
}

/// 待办「完成」按钮：每个待办一行，最多 4 行（QQ 键盘上限）。
fn done_markup(items: &[TodoItem]) -> ReplyMarkup {
    let mut markup = ReplyMarkup::new();
    for item in items.iter().take(4) {
        markup.push_row(vec![MarkupButton::new(
            format!("完成 {}", item.title),
            format!("/完成 {}", item.short_id()),
        )]);
    }
    markup
}

/// 时间在先：依次尝试前缀 1..=3 个空白分隔词作为时间文本，命中后其余为标题。
fn split_when_and_title(
    arg: &str,
    now: i64,
    options: TodoOptions,
) -> Option<(i64, i64, crate::domain::schedule::EventRecurrence, String)> {
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
        "收件箱" | "inbox" => Some(TodoView::Inbox),
        "全部" | "所有" | "all" => Some(TodoView::All),
        _ => None,
    }
}

fn split_title_and_due(arg: &str, now: i64, options: TodoOptions) -> Result<(String, Option<i64>)> {
    let parts: Vec<&str> = arg.split_whitespace().collect();
    if parts.is_empty() {
        bail!("todo title is required");
    }
    for end in 1..parts.len() {
        let candidate = parts[..end].join(" ");
        if let Ok(due_at) = parse_due(&candidate, now, &options) {
            let title = parts[end..].join(" ");
            if !title.trim().is_empty() {
                return Ok((title, Some(due_at)));
            }
        }
    }
    for start in 1..parts.len() {
        let candidate = parts[start..].join(" ");
        if let Ok(due_at) = parse_due(&candidate, now, &options) {
            let title = parts[..start].join(" ");
            if !title.trim().is_empty() {
                return Ok((title, Some(due_at)));
            }
        }
    }
    Ok((parts.join(" "), None))
}

fn split_selector_and_time(arg: &str) -> Option<(&str, &str)> {
    let (selector, mut until) = arg.split_once(char::is_whitespace)?;
    until = until.trim();
    if let Some(rest) = until.strip_prefix('到') {
        until = rest.trim();
    }
    (!selector.is_empty() && !until.is_empty()).then_some((selector, until))
}

fn format_overview(
    overview: &crate::domain::todo::TodoOverview,
    today_events: &[crate::domain::schedule::EventOccurrence],
    now: i64,
    options: TodoOptions,
) -> String {
    if overview.active_count() == 0 && today_events.is_empty() {
        return "## 待办摘要\n\n> 暂无未完成待办与今日行程".to_owned();
    }
    let mut lines = vec![format!(
        "## 待办摘要\n\n> 共 {} 项未完成",
        overview.active_count()
    )];
    push_section(&mut lines, "逾期未完成", &overview.overdue, now, options);
    push_section(&mut lines, "今日", &overview.today, now, options);
    push_section(&mut lines, "近期", &overview.recent, now, options);
    push_section(&mut lines, "长期", &overview.long_term, now, options);
    push_section(&mut lines, "收件箱", &overview.inbox, now, options);
    lines.push(format!("\n### 今日行程（{}）", today_events.len()));
    if today_events.is_empty() {
        lines.push("> 暂无".to_owned());
    } else {
        for occurrence in today_events {
            let mut line = format!(
                "- {} **{}**",
                event_time_label(
                    occurrence.starts_at,
                    occurrence.recurring,
                    options.offset_seconds
                ),
                occurrence.title
            );
            if let Some(location) = &occurrence.location {
                line.push_str(&format!(" @ {location}"));
            }
            lines.push(line);
        }
    }
    lines.join("\n")
}

fn push_section(
    lines: &mut Vec<String>,
    title: &str,
    items: &[TodoItem],
    now: i64,
    options: TodoOptions,
) {
    lines.push(format!("\n### {title}（{}）", items.len()));
    if items.is_empty() {
        lines.push("> 暂无".to_owned());
    } else {
        lines.extend(items.iter().map(|item| format_item(item, now, options)));
    }
}

fn format_items(title: &str, items: &[TodoItem], now: i64, options: TodoOptions) -> String {
    let mut lines = vec![format!("## {title}（{}）", items.len())];
    if items.is_empty() {
        lines.push("\n> 暂无".to_owned());
    } else {
        lines.extend(items.iter().map(|item| format_item(item, now, options)));
    }
    lines.join("\n")
}

fn format_item(item: &TodoItem, now: i64, options: TodoOptions) -> String {
    let label = due_label(item.due_at, now, options, item.status == TodoStatus::Active);
    let priority = match item.priority {
        TodoPriority::High => "，高优先级",
        TodoPriority::Low => "，低优先级",
        TodoPriority::Normal => "",
    };
    format!(
        "- `{}` **{}** — {}{}",
        item.short_id(),
        item.title,
        label,
        priority
    )
}

fn usage() -> String {
    format!(
        "## 待办用法\n\n- `/待办`：今日、逾期、近期、长期摘要（含今日行程）\n- `/待办 <{}>`\n- `/记待办 [时间] <内容>`\n- `/完成 <待办ID>`\n- `/推迟 <待办ID> 到 <时间>`\n- `/取消待办 <待办ID>`\n\n## 日程用法\n\n- `/日程 [今天|明天|N天]`：今日起行程\n- `/记日程 <时间> <标题>`：如 `明天14:00-16:00 班会`、`每周二14:00-15:40 体育课`\n- `/取消日程 <日程ID>`",
        usage_views()
    )
}

fn usage_views() -> String {
    "今日|近期|长期|逾期|收件箱|全部".to_owned()
}
