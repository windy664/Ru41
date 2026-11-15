//! 渠道无关的内置命令：QQ 与 Telegram 共用同一套语义，只有命令词不同。
//!
//! 业务规则（谁能看哪些任务、绑定怎么判、私人数据闸门）全部与渠道无关；
//! 渠道只提供两样东西：命令词（[`Vocabulary`]）与 `CommandContext::reply_channel`。
//!
//! QQ 的历史行为（中文裸词也能命中）由 [`QQ`] 词表保持；Telegram 用 ASCII 词表
//! （[`TELEGRAM`]），因为 Telegram 的命令菜单只接受拉丁字母数字下划线。

use crate::{
    app::{
        identity::{BindOutcome, IdentityService},
        task::TaskService,
    },
    domain::{Task, TaskStatus},
    ports::commands::{CommandContext, MarkupButton, Reply, ReplyMarkup},
};
use anyhow::Result;
use std::sync::Arc;

/// 内置命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    List,
    Show(i64),
    Cancel(i64),
    Bind(String),
    Clear,
    Usage,
    /// 答复交互（审批/提问卡片）：`/答复 <编号前缀> <选项序号>`。
    Judge(String, usize),
}

/// 渠道命令词表：同一语义、不同拼写。
#[derive(Debug, Clone, Copy)]
pub struct Vocabulary {
    /// 无参数命令的整句匹配（可含 `/` 前缀或裸词）。
    pub help: &'static [&'static str],
    pub list: &'static [&'static str],
    pub clear: &'static [&'static str],
    /// 带参数命令的动词（去掉 `/` 后的词）。
    pub show: &'static [&'static str],
    pub cancel: &'static [&'static str],
    pub bind: &'static [&'static str],
    pub usage: &'static [&'static str],
    pub judge: &'static [&'static str],
    /// 展示用主词（帮助页与按钮）：一律带 `/` 前缀。
    pub shown: Display,
}

/// 帮助页与按钮里展示的命令词。
#[derive(Debug, Clone, Copy)]
pub struct Display {
    pub help: &'static str,
    pub list: &'static str,
    pub show: &'static str,
    pub cancel: &'static str,
    pub bind: &'static str,
    pub usage: &'static str,
    pub judge: &'static str,
    pub clear: &'static str,
}

/// QQ 词表：保持历史行为（中文裸词也能命中）。
pub const QQ: Vocabulary = Vocabulary {
    help: &["帮助", "help", "/"],
    list: &["任务", "tasks", "/任务", "/tasks"],
    clear: &["新建", "/新建", "clear", "/clear"],
    show: &["任务", "task", "show"],
    cancel: &["取消", "cancel"],
    bind: &["绑定", "bind"],
    usage: &["用量", "usage"],
    judge: &["答复", "judge"],
    shown: Display {
        help: "/",
        list: "/任务",
        show: "/任务",
        cancel: "/取消",
        bind: "/绑定",
        usage: "/用量",
        judge: "/答复",
        clear: "/新建",
    },
};

/// Telegram 词表：ASCII 命令词（Telegram 命令名只允许拉丁字母数字下划线）。
pub const TELEGRAM: Vocabulary = Vocabulary {
    help: &["/help", "help"],
    list: &["/tasks", "tasks"],
    clear: &["/new", "new"],
    show: &["task", "tasks"],
    cancel: &["cancel"],
    bind: &["bind"],
    usage: &["usage"],
    judge: &["judge"],
    shown: Display {
        help: "/help",
        list: "/tasks",
        show: "/task",
        cancel: "/cancel",
        bind: "/bind",
        usage: "/usage",
        judge: "/judge",
        clear: "/new",
    },
};

const HELP_FOOTER: &str = "---\n> 不必记命令，直接描述任务即可，例如“明天有什么课”或“整理这篇文章”。\n> 当前对话会自动保留上下文；发送 `/新建` 可重新开始。";
const HELP_SECTIONS: &[&str] = &[
    "记录与日程",
    "课程与学习",
    "信息与学习",
    "校园生活",
    "其他能力",
];

/// 内置帮助正文：命令词按渠道渲染（Telegram 用 ASCII，QQ 用历史中文词）。
fn builtin_help(vocabulary: &Vocabulary) -> String {
    let shown = &vocabulary.shown;
    format!(
        "# Ru41 · 个人 AI 助理\n\n\
> 校园事务、学习资料、信息订阅与个人知识库，统一从这里调用。\n\n\
---\n\n\
## 基础指令\n\n\
| 命令 | 用途 |\n\
|---|---|\n\
| `{help}` | 查看这份说明 |\n\
| `{list}` | 最近 10 条任务 |\n\
| `{show} <id>` | 查看任务详情 |\n\
| `{cancel} <id>` | 取消任务 |\n\
| `{usage}` | Token 用量与实际消耗（单聊绑定后可用） |\n\
| `{bind} <口令>` | 绑定个人身份 |\n\
| `{judge} <编号> <序号>` | 答复审批或提问卡片（需绑定） |\n\
| `{clear}` | 开始新对话 |",
        help = shown.help,
        list = shown.list,
        show = shown.show,
        cancel = shown.cancel,
        usage = shown.usage,
        bind = shown.bind,
        judge = shown.judge,
        clear = shown.clear,
    )
}

/// 内置命令与当前启用的扩展命令共同组成帮助文本，扩展命令由插件声明分区。
pub fn help_text(
    handlers: &[Arc<dyn crate::ports::commands::CommandHandler>],
    vocabulary: &Vocabulary,
) -> String {
    let mut text = builtin_help(vocabulary);
    for section in HELP_SECTIONS {
        let mut entries = handlers
            .iter()
            .filter(|handler| handler.help_section() == *section)
            .peekable();
        if entries.peek().is_none() {
            continue;
        }
        text.push_str("\n\n---\n\n## ");
        text.push_str(section);
        for handler in entries {
            text.push_str("\n- ");
            text.push_str(handler.help());
        }
    }
    text.push_str("\n\n");
    text.push_str(HELP_FOOTER);
    text
}

/// 解析内置固定命令；命令词的参数非法时返回 None，让自然语言落入后续路径。
pub fn parse_command(content: &str, vocabulary: &Vocabulary) -> Option<Command> {
    let text = content.trim();
    let lower = text.to_lowercase();
    if vocabulary.help.iter().any(|alias| lower == *alias) {
        return Some(Command::Help);
    }
    if vocabulary.list.iter().any(|alias| lower == *alias) {
        return Some(Command::List);
    }
    if vocabulary.clear.iter().any(|alias| lower == *alias) {
        return Some(Command::Clear);
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let verb = parts.next()?.to_lowercase();
    let verb = verb.strip_prefix('/').unwrap_or(&verb);
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    if vocabulary.usage.contains(&verb) {
        return Some(Command::Usage);
    }
    if vocabulary.show.contains(&verb) {
        return Some(Command::Show(arg?.parse().ok()?));
    }
    if vocabulary.cancel.contains(&verb) {
        return Some(Command::Cancel(arg?.parse().ok()?));
    }
    if vocabulary.bind.contains(&verb) {
        return Some(Command::Bind(arg?.to_owned()));
    }
    if vocabulary.judge.contains(&verb) {
        let mut pair = arg?.split_whitespace();
        let id_prefix = pair.next()?;
        let option: usize = pair.next()?.parse().ok()?;
        return (option >= 1).then(|| Command::Judge(id_prefix.to_owned(), option));
    }
    None
}

/// 执行内置命令并返回回复文本；不直接发消息，便于测试。
pub fn execute(
    command: &Command,
    ctx: &CommandContext,
    pricing: &crate::app::usage_report::Pricing,
    vocabulary: &Vocabulary,
) -> Result<Reply> {
    let identity = IdentityService::new(ctx.store.clone());
    let bound = identity.is_bound(ctx.reply_channel, ctx.author_openid)?;
    let aggregate = ctx.is_c2c && bound;
    let visible = |task: &Task| {
        let Some(notify) = &task.notify else {
            return false;
        };
        if notify.channel != ctx.reply_channel {
            return false;
        }
        notify.target == ctx.target || aggregate
    };
    match command {
        Command::Help => Ok(Reply::with_markup(
            help_text(&[], vocabulary),
            help_markup(vocabulary),
        )),
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
                task_rows_markup(&tasks, vocabulary),
            ))
        }
        Command::Show(id) => match ctx.store.get(*id)? {
            Some(task) if visible(&task) => Ok(Reply::with_markup(
                format_task_detail(&task),
                ReplyMarkup::new().row(vec![MarkupButton::new(
                    "取消任务",
                    format!("{} {}", vocabulary.shown.cancel, task.id),
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
            Some(expected) => match identity.bind(
                ctx.reply_channel,
                ctx.author_openid,
                expected,
                given,
                crate::util::now(),
            )? {
                BindOutcome::Bound => Ok(Reply::text(format!(
                    "## 绑定成功\n\n当前 {} 已绑定，可在单聊中跨会话查询任务。",
                    ctx.reply_channel
                ))),
                BindOutcome::Rejected => Ok(Reply::text("## 绑定失败\n\n口令错误，请重新发送。")),
                BindOutcome::Locked => Ok(Reply::text(
                    "## 尝试过于频繁\n\n连续多次口令错误，请 10 分钟后再试。",
                )),
            },
        },
        Command::Judge(id_prefix, option) => {
            // 答复权只给绑定用户：交互可能来自群聊卡片，不能谁点都算数。
            if !bound {
                return Ok(Reply::text(
                    "## 需要身份绑定\n\n请先发送 `/绑定 <口令>`，再答复。",
                ));
            }
            judge(ctx.store, id_prefix, *option)
        }
        Command::Usage => {
            // 用量是全局个人数据，只在绑定身份的单聊里开放。
            if !aggregate {
                return Ok(Reply::text(
                    "## 用量未开放\n\n请先在单聊中用 `/绑定 <口令>` 绑定身份后再查询。",
                ));
            }
            let now = crate::util::now();
            let day = crate::app::usage::overview(&ctx.store.usage_runs_since(now - 86_400)?, 1);
            let week =
                crate::app::usage::overview(&ctx.store.usage_runs_since(now - 7 * 86_400)?, 7);
            Ok(Reply::text(crate::app::usage_report::render(
                &week,
                &day,
                crate::app::usage_report::Style::Chat,
                pricing,
            )))
        }
        Command::Clear => {
            // 会话键与 worker 侧一致：`{channel}:{target}`。
            let key = format!("{}:{}", ctx.reply_channel, ctx.target);
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

/// 答复执行：按编号前缀定位 pending 交互，把对应选项写回。
/// 过期/已答复/序号越界都给人话回执，不吞错误。
fn judge(
    store: &Arc<dyn crate::ports::storage::TaskStore>,
    id_prefix: &str,
    option: usize,
) -> Result<Reply> {
    let docs = store.documents("interaction", 100)?;
    let mut matches: Vec<crate::domain::records::Interaction> = docs
        .into_iter()
        .filter_map(|doc| serde_json::from_value(doc.value).ok())
        .filter(|interaction: &crate::domain::records::Interaction| {
            interaction.id.starts_with(id_prefix)
        })
        .collect();
    let Some(interaction) = matches.pop() else {
        return Ok(Reply::text(format!(
            "## 答复失败\n\n没有找到编号 `{id_prefix}` 的交互，它可能已过期。"
        )));
    };
    if interaction.status != "pending" {
        return Ok(Reply::text(format!(
            "## 已处理\n\n交互 `{id_prefix}` 当前状态：{}，无需重复答复。",
            interaction.status
        )));
    }
    let Some(chosen) = interaction.options.get(option - 1) else {
        return Ok(Reply::text(format!(
            "## 序号越界\n\n交互 `{id_prefix}` 只有 {} 个选项。",
            interaction.options.len()
        )));
    };
    let label = crate::app::interactions::option_label(chosen);
    crate::app::interactions::InteractionService {
        store: store.clone(),
    }
    .reply(&interaction.id, chosen.clone())?;
    Ok(Reply::text(format!(
        "## ✅ 已答复\n\n- **编号**：`{id_prefix}`\n- **你的选择**：{label}\n\n任务已继续执行。"
    )))
}

/// 帮助的快捷按钮：常用入口两行。内置命令按渠道词表拼，扩展命令两渠道通用。
pub fn help_markup(vocabulary: &Vocabulary) -> ReplyMarkup {
    let shown = &vocabulary.shown;
    ReplyMarkup::new()
        .row(vec![
            MarkupButton::new("待办", "#"),
            MarkupButton::new("日程", "/日程"),
            MarkupButton::new("任务", shown.list),
        ])
        .row(vec![
            MarkupButton::new("帮助", shown.help),
            MarkupButton::new("新建对话", shown.clear),
        ])
}

/// 任务列表按钮：每个任务一行「详情 / 取消」，最多 4 行（键盘宽度上限）。
pub fn task_rows_markup(tasks: &[Task], vocabulary: &Vocabulary) -> ReplyMarkup {
    let shown = &vocabulary.shown;
    let mut markup = ReplyMarkup::new();
    for task in tasks.iter().take(4) {
        markup.push_row(vec![
            MarkupButton::new(
                format!("详情 #{}", task.id),
                format!("{} {}", shown.show, task.id),
            ),
            MarkupButton::new(
                format!("取消 #{}", task.id),
                format!("{} {}", shown.cancel, task.id),
            ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qq_vocabulary_keeps_legacy_behaviour() {
        for input in ["帮助", "help", "/", " / "] {
            assert_eq!(parse_command(input, &QQ), Some(Command::Help));
        }
        for input in ["任务", "/任务", "tasks", "/tasks"] {
            assert_eq!(parse_command(input, &QQ), Some(Command::List));
        }
        for input in ["任务 3", "/任务 3", "task 3", "/task 3", "show 3"] {
            assert_eq!(parse_command(input, &QQ), Some(Command::Show(3)));
        }
        assert_eq!(parse_command("/帮助", &QQ), None);
        assert_eq!(parse_command("任务怎么做", &QQ), None);
        assert_eq!(parse_command("取消", &QQ), None);
        assert_eq!(parse_command("绑定", &QQ), None);
    }

    #[test]
    fn telegram_vocabulary_uses_ascii_commands() {
        assert_eq!(parse_command("/help", &TELEGRAM), Some(Command::Help));
        assert_eq!(parse_command("/tasks", &TELEGRAM), Some(Command::List));
        assert_eq!(parse_command("/task 7", &TELEGRAM), Some(Command::Show(7)));
        assert_eq!(
            parse_command("/cancel 7", &TELEGRAM),
            Some(Command::Cancel(7))
        );
        assert_eq!(
            parse_command("/bind secret", &TELEGRAM),
            Some(Command::Bind("secret".into()))
        );
        assert_eq!(parse_command("/new", &TELEGRAM), Some(Command::Clear));
        // 中文命令词不属于 Telegram 内置命令（插件命令仍可命中）。
        assert_eq!(parse_command("/任务", &TELEGRAM), None);
    }
}
