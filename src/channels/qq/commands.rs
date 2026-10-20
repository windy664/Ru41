use crate::{
    app::{
        identity::{BindOutcome, IdentityService},
        task::TaskService,
    },
    domain::{Task, TaskStatus},
    ports::{
        commands::{CommandContext, MarkupButton, Reply, ReplyMarkup},
        storage::TaskStore,
    },
};
use anyhow::Result;
use std::sync::Arc;

/// QQ 内置固定命令：精确命中才拦截，其余消息先尝试扩展命令，再入队给 Agent。
// TODO: 审批命令（审批/裁决）待核心的 Interaction 回复与租约原子性、崩溃恢复
// 完成后再接入（docs/core-status.md 未完成清单第 1、2 条）；届时仅绑定用户可裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    List,
    Show(i64),
    Cancel(i64),
    Bind(String),
    Clear,
}

const BUILTIN_HELP: &str = "## Ru41 机器人

**基础指令**
- `/帮助` 或 `/help`：查看这份说明
- `/任务` 或 `/tasks`：最近 10 条任务
- `/任务 <id>` 或 `/task <id>`：查看任务详情
- `/取消 <id>` 或 `/cancel <id>`：取消任务
- `/绑定 <口令>` 或 `/bind <口令>`：绑定个人身份
- `/新建` 或 `/clear`：开始新对话";

const HELP_FOOTER: &str = "> 直接描述你的任务即可，当前对话会自动保留上下文。";

/// 内置命令与装配层注册的扩展命令共同组成帮助文本。
pub fn help_text(handlers: &[Arc<dyn crate::ports::commands::CommandHandler>]) -> String {
    let mut text = BUILTIN_HELP.to_owned();
    if !handlers.is_empty() {
        text.push_str("\n\n**扩展指令**");
        for handler in handlers {
            text.push_str("\n- ");
            text.push_str(handler.help());
        }
    }
    text.push_str("\n\n");
    text.push_str(HELP_FOOTER);
    text
}

/// 解析内置固定命令；命令词的参数非法时返回 None，让自然语言落入后续路径。
pub fn parse_command(content: &str) -> Option<Command> {
    let text = content.trim();
    let lower = text.to_lowercase();
    if matches!(lower.as_str(), "帮助" | "help" | "/帮助" | "/help") {
        return Some(Command::Help);
    }
    if matches!(lower.as_str(), "任务" | "tasks" | "/任务" | "/tasks") {
        return Some(Command::List);
    }
    if matches!(lower.as_str(), "新建" | "/新建" | "clear" | "/clear") {
        return Some(Command::Clear);
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let verb = parts.next()?.to_lowercase();
    let verb = verb.strip_prefix('/').unwrap_or(&verb);
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match verb {
        "任务" | "task" | "show" => Some(Command::Show(arg?.parse().ok()?)),
        "取消" | "cancel" => Some(Command::Cancel(arg?.parse().ok()?)),
        "绑定" | "bind" => Some(Command::Bind(arg?.to_owned())),
        _ => None,
    }
}

/// 执行内置命令并返回回复文本；不直接发消息，便于测试。
pub fn execute(command: &Command, ctx: &CommandContext) -> Result<Reply> {
    let identity = IdentityService::new(ctx.store.clone());
    let bound = identity.is_bound("qq", ctx.author_openid)?;
    let aggregate = ctx.is_c2c && bound;
    let visible = |task: &Task| {
        let Some(notify) = &task.notify else {
            return false;
        };
        if notify.channel != "qq" {
            return false;
        }
        notify.target == ctx.target || aggregate
    };
    match command {
        Command::Help => Ok(Reply::with_markup(help_text(&[]), help_markup())),
        Command::List => {
            let tasks: Vec<Task> = ctx
                .store
                .list(100)?
                .into_iter()
                .filter(visible)
                .take(10)
                .collect();
            Ok(Reply::with_markup(
                format_task_list(&tasks),
                task_rows_markup(&tasks),
            ))
        }
        Command::Show(id) => match ctx.store.get(*id)? {
            Some(task) if visible(&task) => Ok(Reply::with_markup(
                format_task_detail(&task),
                ReplyMarkup::new().row(vec![MarkupButton::new(
                    "取消任务",
                    format!("/取消 {}", task.id),
                )]),
            )),
            _ => Ok(Reply::text(format!("任务 #{id} 不存在或无权查看"))),
        },
        Command::Cancel(id) => match ctx.store.get(*id)? {
            Some(task) if visible(&task) => {
                let task = TaskService::new(ctx.store.clone()).cancel(*id)?;
                Ok(Reply::text(format!(
                    "## 任务已取消\n\n- **任务**：`#{}`\n- **原状态**：{}",
                    task.id,
                    status_label(task.status)
                )))
            }
            _ => Ok(Reply::text(format!("任务 #{id} 不存在或无权操作"))),
        },
        Command::Bind(given) => match ctx.bind_secret {
            None => Ok(Reply::text("## 绑定未开放\n\n当前未配置绑定口令。")),
            Some(expected) => {
                match identity.bind("qq", ctx.author_openid, expected, given, crate::util::now())? {
                    BindOutcome::Bound => Ok(Reply::text(
                        "## 绑定成功\n\n当前 QQ 已绑定，可在单聊中跨会话查询任务。",
                    )),
                    BindOutcome::Rejected => {
                        Ok(Reply::text("## 绑定失败\n\n口令错误，请重新发送。"))
                    }
                    BindOutcome::Locked => Ok(Reply::text(
                        "## 尝试过于频繁\n\n连续多次口令错误，请 10 分钟后再试。",
                    )),
                }
            }
        },
        Command::Clear => {
            let key = format!("qq:{}", ctx.target);
            if let Some(doc) = ctx.store.document("conversation", &key)? {
                let mut value = doc.value.clone();
                value["session_id"] = serde_json::Value::Null;
                if !ctx
                    .store
                    .put_document("conversation", &key, Some(doc.revision), &value)?
                {
                    return Ok(Reply::text("## 请稍后重试\n\n会话正在更新。"));
                }
            }
            Ok(Reply::text(
                "## 新对话已开始\n\n后续消息将不再携带上一轮上下文。",
            ))
        }
    }
}

/// /帮助 的快捷按钮：常用入口两行。
fn help_markup() -> ReplyMarkup {
    ReplyMarkup::new()
        .row(vec![
            MarkupButton::new("待办", "/待办"),
            MarkupButton::new("日程", "/日程"),
            MarkupButton::new("任务", "/任务"),
        ])
        .row(vec![
            MarkupButton::new("帮助", "/帮助"),
            MarkupButton::new("新建对话", "/新建"),
        ])
}

/// 任务列表按钮：每个任务一行「详情 / 取消」，最多 4 行（QQ 键盘上限）。
fn task_rows_markup(tasks: &[Task]) -> ReplyMarkup {
    let mut markup = ReplyMarkup::new();
    for task in tasks.iter().take(4) {
        markup.push_row(vec![
            MarkupButton::new(format!("详情 #{}", task.id), format!("/任务 {}", task.id)),
            MarkupButton::new(format!("取消 #{}", task.id), format!("/取消 {}", task.id)),
        ]);
    }
    markup
}

pub fn format_task_list(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "## 任务列表\n\n> 暂无可见任务".to_owned();
    }
    let mut lines = vec![format!("## 最近任务（{} 条）", tasks.len())];
    for task in tasks {
        lines.push(format!(
            "- `#{}` **{}** — {}",
            task.id,
            status_label(task.status),
            truncate(&task.prompt, 20)
        ));
    }
    lines.join("\n")
}

pub fn format_task_detail(task: &Task) -> String {
    let mut lines = vec![
        format!("## 任务 `#{}`", task.id),
        format!("- **状态**：{}", status_label(task.status)),
        format!("- **底座**：`{}`", task.backend_id),
        format!("- **创建**：{}", task.created_at),
        format!("- **内容**：{}", truncate(&task.prompt, 100)),
    ];
    if let Some(result) = &task.result {
        lines.push(format!("\n**结果**\n{}", result.summary));
    } else if let Some(error) = &task.last_error {
        lines.push(format!("\n**错误**\n{error}"));
    }
    lines.join("\n")
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "排队中",
        TaskStatus::Running => "运行中",
        TaskStatus::RetryScheduled => "待重试",
        TaskStatus::Succeeded => "已完成",
        TaskStatus::Failed => "失败",
        TaskStatus::Cancelled => "已取消",
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_owned();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push('…');
    out
}
