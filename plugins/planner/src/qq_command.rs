use anyhow::{Result, ensure};
use ru41::{
    app::{schedule::ScheduleService, todo::TodoService},
    domain::{
        schedule::{EventInput, EventSource, event_time_label},
        todo::{TodoInput, TodoItem, TodoOptions, TodoPriority, TodoSource, TodoView},
    },
    ports::{
        commands::{CommandContext, Reply, SimpleCommand},
        storage::TaskStore,
    },
};
use std::sync::Arc;

use crate::{
    parse::{
        extract_note, parse_event_view, split_leading_time, split_selector_and_time,
        split_when_and_title,
    },
    view::{cmd_input_chip, format_items, usage},
};

/// QQ 私人待办入口；持久化与日期视图由核心 TodoService 提供。
/// `jwxt_semester_start` 来自 jwxt 模块配置，/日程 用它把课表并进同一条时间轴。
pub struct TodoQqCommand {
    options: TodoOptions,
    jwxt_semester_start: Option<chrono::NaiveDate>,
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
        "`#内容`：记入待办（开头带时间词自动排期）；单独 `#`：全部待办清单；`/日程`：今日与未来行程"
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
                        "已取消日程\n\n- **{}**\n- **ID**：`{}`",
                        item.title,
                        item.short_id()
                    ))
                }
                "完成待办" | "完成" | "done" => {
                    ensure!(!arg.is_empty(), "用法：`/完成 <待办ID>`");
                    service.complete(arg, now)?;
                    // 只回清单：完成的那条已经在下面划横线区，不用再提示一遍。
                    todo_list_reply(&service, now, self.options)?
                }
                "推迟待办" | "推迟" => {
                    let (selector, until) = split_selector_and_time(arg)
                        .ok_or_else(|| anyhow::anyhow!("用法：`/推迟 <待办ID> 到 <时间>`"))?;
                    service.postpone(selector, until, now)?;
                    todo_list_reply(&service, now, self.options)?
                }
                "取消待办" => {
                    ensure!(!arg.is_empty(), "用法：`/取消待办 <待办ID>`");
                    service.cancel(arg, now)?;
                    todo_list_reply(&service, now, self.options)?
                }
                "恢复待办" | "restore" => {
                    ensure!(!arg.is_empty(), "用法：`/恢复待办 <待办ID>`");
                    service.restore(arg, now)?;
                    todo_list_reply(&service, now, self.options)?
                }
                _ => Reply::text(usage()),
            };
            Ok(Some(reply))
        }
    }

    /// 自然语言捕获：整句以 `#` 或触发前缀开头即视为记录意图。
    /// 单独一个 `#`：列出全部未完成。
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
            return self.list_reply(&service, now).map(Some);
        }
        Ok(Some(self.quick_capture_reply(&service, &note, ctx, now)?))
    }
}

/// 待办类操作的统一回执：只给完整清单（未完成 + 近 7 天已完成划横线）。
/// 不加「已记入/已完成」这类提示行——清单本身已经反映了状态。
fn todo_list_reply(service: &TodoService, now: i64, options: TodoOptions) -> Result<Reply> {
    let items: Vec<TodoItem> = service.list(TodoView::All, now, 50)?;
    Ok(Reply::text(format_items("待办", &items, now, options)))
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
        service.add(
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
        // 只回清单：新条目就在清单里，不用再提示一遍。
        Ok(todo_list_reply(&service, now, self.options)?)
    }

    fn list_reply(&self, service: &TodoService, now: i64) -> Result<Reply> {
        // 唯一清单视图：未完成（按截止时间排序）+ 底部近 7 天已完成。
        let items: Vec<TodoItem> = service.list(TodoView::All, now, 50)?;
        Ok(Reply::text(format_items("待办", &items, now, self.options)))
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
            "已添加日程\n\n- **{}**\n- **时间**：{} — {}\n- **ID**：`{}`\n- 查看：`/日程`",
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
            "**日程（未来 {} 天，行程 {} 项）**",
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
