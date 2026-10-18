use crate::{
    app::{identity::IdentityService, task::TaskService},
    domain::{Task, TaskStatus},
    ports::storage::TaskStore,
};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// QQ 扩展命令：由业务模块实现、装配层注册，QQ 渠道本身不认识具体业务。
/// 返回 Ok(None) 表示不拦截（如参数非法），消息继续按自然语言入队给 Agent。
pub trait QqCommandHandler: Send + Sync {
    /// 是否匹配命令词；verb 已转为小写。
    fn matches(&self, verb: &str) -> bool;
    /// 单行帮助文本。
    fn help(&self) -> &'static str;
    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send + 'a>>;
}

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
}

const BUILTIN_HELP: &str = "可用命令：
帮助 - 显示本说明
任务 - 列出本会话最近任务
任务 <id> - 查看任务详情
取消 <id> - 取消任务
绑定 <口令> - 绑定个人身份（绑定后单聊可跨会话查询）";

const HELP_FOOTER: &str = "其他消息将作为任务交给 Agent 处理";

/// 内置命令与装配层注册的扩展命令共同组成帮助文本。
pub fn help_text(handlers: &[Arc<dyn QqCommandHandler>]) -> String {
    let mut text = BUILTIN_HELP.to_owned();
    for handler in handlers {
        text.push('\n');
        text.push_str(handler.help());
    }
    text.push('\n');
    text.push_str(HELP_FOOTER);
    text
}

/// 解析内置固定命令；命令词的参数非法时返回 None，让自然语言落入后续路径。
pub fn parse_command(content: &str) -> Option<Command> {
    let text = content.trim();
    let lower = text.to_lowercase();
    if matches!(lower.as_str(), "帮助" | "help" | "/help") {
        return Some(Command::Help);
    }
    if matches!(lower.as_str(), "任务" | "tasks" | "/tasks") {
        return Some(Command::List);
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let verb = parts.next()?.to_lowercase();
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match verb.as_str() {
        "任务" | "task" | "show" => Some(Command::Show(arg?.parse().ok()?)),
        "取消" | "cancel" => Some(Command::Cancel(arg?.parse().ok()?)),
        "绑定" | "bind" => Some(Command::Bind(arg?.to_owned())),
        _ => None,
    }
}

/// 命令执行上下文：渠道适配器只负责收发，业务判断集中在命令实现里。
pub struct CommandContext<'a> {
    pub store: &'a Arc<dyn TaskStore>,
    /// 未配置口令时绑定功能关闭。
    pub bind_secret: Option<&'a str>,
    /// 当前会话的通知 target（group:{openid} 或 user:{openid}）。
    pub target: &'a str,
    /// 当前消息 ID，扩展命令可用它登记终态被动回复路由。
    pub msg_id: &'a str,
    /// 发消息用户的 openid，用于绑定状态判定。
    pub author_openid: &'a str,
    /// 是否单聊；只有单聊且已绑定时才聚合该用户跨会话的任务。
    pub is_c2c: bool,
}

/// 执行内置命令并返回回复文本；不直接发消息，便于测试。
pub fn execute(command: &Command, ctx: &CommandContext) -> Result<String> {
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
        Command::Help => Ok(help_text(&[])),
        Command::List => {
            let tasks: Vec<Task> = ctx
                .store
                .list(100)?
                .into_iter()
                .filter(visible)
                .take(10)
                .collect();
            Ok(format_task_list(&tasks))
        }
        Command::Show(id) => match ctx.store.get(*id)? {
            Some(task) if visible(&task) => Ok(format_task_detail(&task)),
            _ => Ok(format!("任务 #{id} 不存在或无权查看")),
        },
        Command::Cancel(id) => match ctx.store.get(*id)? {
            Some(task) if visible(&task) => {
                let task = TaskService::new(ctx.store.clone()).cancel(*id)?;
                Ok(format!(
                    "任务 #{} 已取消（原状态：{}）",
                    task.id,
                    status_label(task.status)
                ))
            }
            _ => Ok(format!("任务 #{id} 不存在或无权操作")),
        },
        Command::Bind(given) => match ctx.bind_secret {
            None => Ok("绑定功能未开放（未配置绑定口令）".to_owned()),
            Some(expected) if identity.bind("qq", ctx.author_openid, expected, given)? => {
                Ok("绑定成功，单聊中可跨会话查询任务".to_owned())
            }
            Some(_) => Ok("口令错误，绑定失败".to_owned()),
        },
    }
}

pub fn format_task_list(tasks: &[Task]) -> String {
    if tasks.is_empty() {
        return "暂无可见任务".to_owned();
    }
    let mut lines = vec![format!("最近任务（{} 条）：", tasks.len())];
    for task in tasks {
        lines.push(format!(
            "#{} [{}] {}",
            task.id,
            status_label(task.status),
            truncate(&task.prompt, 20)
        ));
    }
    lines.join("\n")
}

pub fn format_task_detail(task: &Task) -> String {
    let mut lines = vec![
        format!("任务 #{}", task.id),
        format!("状态：{}", status_label(task.status)),
        format!("底座：{}", task.backend_id),
        format!("创建：{}", task.created_at),
        format!("内容：{}", truncate(&task.prompt, 100)),
    ];
    if let Some(result) = &task.result {
        lines.push(format!("结果：{}", result.summary));
    } else if let Some(error) = &task.last_error {
        lines.push(format!("错误：{error}"));
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
