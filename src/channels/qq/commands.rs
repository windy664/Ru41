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
    Usage,
}

const BUILTIN_HELP: &str = "## Ru41 机器人

**基础指令**
- `/帮助` 或 `/help`：查看这份说明
- `/任务` 或 `/tasks`：最近 10 条任务
- `/任务 <id>` 或 `/task <id>`：查看任务详情
- `/取消 <id>` 或 `/cancel <id>`：取消任务
- `/用量` 或 `/usage`：token 用量统计（单聊绑定后可用）
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
    if matches!(verb, "用量" | "usage") {
        return Some(Command::Usage);
    }
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
            Ok(Reply::text(format_usage_overview(&day, &week)))
        }
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

/// token 数的人类可读格式：1.2M / 45.6K / 1234。
fn human_tokens(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 10_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn totals(summaries: &[crate::app::usage::UsageSummary]) -> (u32, u64) {
    summaries.iter().fold((0, 0), |(runs, tokens), item| {
        (runs + item.runs, tokens + item.total_tokens)
    })
}

/// 新版用量渲染：以运行次数为主指标，token 为副；三种分组 + 覆盖率说明。
///
/// QQ 的 markdown 不支持表格，用分组标题 + 缩进列表呈现。
pub fn format_usage_overview(
    day: &crate::app::usage::UsageOverview,
    week: &crate::app::usage::UsageOverview,
) -> String {
    let mut lines = vec!["## 📊 Token 用量".to_owned()];
    lines.push(String::new());
    lines.push(format!(
        "**近 24 小时** · {} 次运行 · {} 次上报 · {}",
        day.runs,
        day.reported_runs,
        token_phrase(day.total_tokens, day.reported_runs)
    ));
    lines.push(format!(
        "**近 7 天** · {} 次运行 · {} 次上报 · {}",
        week.runs,
        week.reported_runs,
        token_phrase(week.total_tokens, week.reported_runs)
    ));
    if week.reported_runs > 0 {
        lines.push(format!(
            "　└ 输入 {} · 缓存命中 {} · 输出 {}",
            human_tokens(week.input_tokens),
            human_tokens(week.cached_input_tokens),
            human_tokens(week.output_tokens)
        ));
    }
    // 成功率与耗时：不依赖底座是否上报 token，永远有——报告再"空"也有硬数据。
    lines.push(format!(
        "**成功率** · {}/{} 次成功 · 失败 {} 次 · 平均 {:.0}s（最长 {:.0}s）",
        week.succeeded, week.runs, week.failed, week.avg_seconds as f64, week.max_seconds as f64
    ));

    if !week.by_model.is_empty() {
        lines.push(String::new());
        lines.push("**按模型**".to_owned());
        for item in &week.by_model {
            lines.push(format!(
                "- `{}` · {} 次 · 成功 {} · {}",
                item.key,
                item.runs,
                item.succeeded,
                token_phrase(item.total_tokens, item.reported_runs)
            ));
        }
    }
    if !week.by_source.is_empty() {
        lines.push(String::new());
        lines.push("**按功能来源**".to_owned());
        for item in &week.by_source {
            let label = crate::app::usage::source_label(&item.key);
            let name = if label.is_empty() {
                item.key.clone()
            } else {
                format!("{label}（{}）", item.key)
            };
            lines.push(format!(
                "- {name} · {} 次 · 成功 {} · {}",
                item.runs,
                item.succeeded,
                token_phrase(item.total_tokens, item.reported_runs)
            ));
        }
    }
    if !week.by_day.is_empty() {
        lines.push(String::new());
        lines.push("**近 7 天趋势**".to_owned());
        for item in &week.by_day {
            lines.push(format!(
                "- {} · {} 次 · {}",
                item.key,
                item.runs,
                token_phrase(item.total_tokens, item.reported_runs)
            ));
        }
    }
    if week.runs > week.reported_runs {
        lines.push(String::new());
        lines.push(format!(
            "> ℹ️ {} 次运行没有 token 数据：codex 走 GLM 的 chat 接口不返回 usage（只报 0），所以 token 列不出来；运行次数、成功率、耗时都是实的。\n想看某次运行具体做了什么：`ru41 task trace <任务号>`",
            week.runs - week.reported_runs
        ));
    }
    lines.join("\n")
}

/// token 部分的措辞：没有数据时说清"未上报"，不要显示 0 冒充真实值。
fn token_phrase(total: u64, reported_runs: u32) -> String {
    if reported_runs == 0 {
        "未上报 token".to_owned()
    } else {
        format!("{} token", human_tokens(total))
    }
}

pub fn format_usage(
    day: &[crate::app::usage::UsageSummary],
    week: &[crate::app::usage::UsageSummary],
    models: &[crate::app::usage::ModelUsage],
) -> String {
    let (day_runs, day_tokens) = totals(day);
    let (week_runs, week_tokens) = totals(week);
    let (week_input, week_cached, week_output) = split_totals(week);
    let mut lines = vec![
        "## Token 用量".to_owned(),
        format!(
            "- **最近 24 小时**：{} tokens（{} 次）",
            human_tokens(day_tokens),
            day_runs
        ),
        format!(
            "- **最近 7 天**：{} tokens（{} 次）",
            human_tokens(week_tokens),
            week_runs
        ),
        format!(
            "- 其中：输入 {} / 缓存命中 {} / 输出 {}",
            human_tokens(week_input),
            human_tokens(week_cached),
            human_tokens(week_output)
        ),
    ];
    if !models.is_empty() {
        lines.push(String::new());
        lines.push("**7 天分模型**（每个模型各花了多少）".to_owned());
        for item in models {
            lines.push(format!(
                "- `{}`：{}（{} 次）｜输入 {}、缓存 {}、输出 {}",
                item.model,
                human_tokens(item.total_tokens),
                item.runs,
                human_tokens(item.input_tokens),
                human_tokens(item.cached_input_tokens),
                human_tokens(item.output_tokens)
            ));
        }
    }
    if !week.is_empty() {
        lines.push(String::new());
        lines.push("**7 天按功能来源**（哪类任务发起的）".to_owned());
        for item in week {
            lines.push(format!(
                "- `{}` {}：{}（{} 次）",
                item.source,
                source_hint(&item.source),
                human_tokens(item.total_tokens),
                item.runs
            ));
        }
    }
    lines.push(String::new());
    lines.push(
        "> 缓存命中的输入按更低单价计费，不等于花了同等的钱；底座未上报用量的运行不计入。"
            .to_owned(),
    );
    lines.join("\n")
}

/// 来源前缀的中文说明：让人知道这一行是哪类任务。
fn source_hint(source: &str) -> &str {
    match source {
        "qq" => "（QQ 消息）",
        "lecture" => "（课堂录音笔记）",
        "obsidian" => "（Obsidian 整理）",
        "digest" => "（每日资讯摘要）",
        "youtube" => "（YouTube 精听）",
        "probe" => "（临时测试，可忽略）",
        _ => "",
    }
}

/// 输入 / 缓存命中 / 输出 三个口径的合计。
fn split_totals(summaries: &[crate::app::usage::UsageSummary]) -> (u64, u64, u64) {
    summaries
        .iter()
        .fold((0, 0, 0), |(input, cached, output), item| {
            (
                input + item.input_tokens,
                cached + item.cached_input_tokens,
                output + item.output_tokens,
            )
        })
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
    use crate::app::usage::{ModelUsage, UsageSummary};

    fn summary(source: &str, total: u64, input: u64, cached: u64, output: u64) -> UsageSummary {
        UsageSummary {
            source: source.to_owned(),
            runs: 1,
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            total_tokens: total,
        }
    }

    #[test]
    fn usage_overview_shows_runs_coverage_and_unreported() {
        use crate::app::usage::{UsageOverview, UsageStat};
        let stat = |key: &str, runs: u32, reported: u32, total: u64| UsageStat {
            key: key.to_owned(),
            runs,
            reported_runs: reported,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: total,
            succeeded: runs.saturating_sub(1),
            failed: 1,
        };
        let week = UsageOverview {
            days: 7,
            runs: 123,
            reported_runs: 1,
            input_tokens: 154,
            cached_input_tokens: 10_500,
            output_tokens: 850,
            total_tokens: 11_500,
            by_model: vec![stat("minimax/MiniMax-M3", 6, 1, 11_500)],
            by_source: vec![stat("qq", 6, 1, 11_500), stat("lecture", 24, 0, 0)],
            by_day: vec![stat("11-05", 30, 1, 11_500)],
            succeeded: 120,
            failed: 3,
            avg_seconds: 42,
            max_seconds: 300,
        };
        let day = UsageOverview {
            days: 1,
            runs: 30,
            reported_runs: 1,
            input_tokens: 154,
            cached_input_tokens: 10_500,
            output_tokens: 850,
            total_tokens: 11_500,
            by_model: Vec::new(),
            by_source: Vec::new(),
            by_day: Vec::new(),
            succeeded: 29,
            failed: 1,
            avg_seconds: 40,
            max_seconds: 280,
        };
        let text = format_usage_overview(&day, &week);
        eprintln!("{text}");
        // 运行次数是主指标。
        assert!(text.contains("123 次运行"), "{text}");
        assert!(text.contains("11.5K token"), "{text}");
        // 没上报的一组要说清"未上报"，不能显示 0。
        assert!(text.contains("未上报 token"), "{text}");
        // 覆盖率与追查入口。
        assert!(text.contains("122 次运行没有 token 数据"), "{text}");
        // 成功率与耗时：不依赖 token 覆盖，永远有。
        assert!(text.contains("成功率"), "{text}");
        assert!(text.contains("平均"), "{text}");
        assert!(text.contains("ru41 task trace"), "{text}");
        // 来源分组带中文名。
        assert!(text.contains("QQ 消息（qq）"), "{text}");
        assert!(text.contains("课堂录音整理（lecture）"), "{text}");
    }

    #[test]
    fn usage_report_shows_input_cache_and_output() {
        let week = vec![summary("qq", 11500, 154, 10496, 850)];
        let models = vec![ModelUsage {
            model: "minimax/MiniMax-M3".into(),
            runs: 1,
            input_tokens: 154,
            cached_input_tokens: 10496,
            output_tokens: 850,
            total_tokens: 11500,
        }];
        let text = format_usage(&week, &week, &models);
        // 三个口径必须分开显示：只给总数会让人以为一次花了 1.1 万。
        assert!(text.contains("输入 154"), "{text}");
        assert!(text.contains("缓存命中 10.5K"), "{text}");
        assert!(text.contains("输出 850"), "{text}");
        assert!(text.contains("minimax/MiniMax-M3"), "{text}");
        // 来源要说明是哪类任务。
        assert!(text.contains("按功能来源"), "{text}");
        assert!(text.contains("QQ 消息"), "{text}");
    }
}
