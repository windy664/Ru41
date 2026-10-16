use crate::{
    app::{identity::IdentityService, task::TaskService},
    domain::{Task, TaskStatus},
    ports::storage::TaskStore,
};
use anyhow::Result;
use std::sync::Arc;

/// QQ 固定命令：精确命中才拦截，其余消息照常入队给 Agent。
// TODO: 审批命令（审批/裁决）待核心的 Interaction 回复与租约原子性、崩溃恢复
// 完成后再接入（docs/core-status.md 未完成清单第 1、2 条）；届时仅绑定用户可裁决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    List,
    Show(i64),
    Cancel(i64),
    Bind(String),
    Utility,
    /// 精读指定 arXiv 论文；由 QqBot 异步处理（需要 arXiv 客户端），不进 execute。
    DeepRead(String),
}

pub const HELP_TEXT: &str = "可用命令：
帮助 - 显示本说明
任务 - 列出本会话最近任务
任务 <id> - 查看任务详情
取消 <id> - 取消任务
精读 <arxiv-id> - 精读指定论文（如 精读 2510.12345）
绑定 <口令> - 绑定个人身份（绑定后单聊可跨会话查询）
水电 - 查询宿舍水电余额（需绑定身份，仅单聊）
其他消息将作为任务交给 Agent 处理";

/// arXiv 新格式 ID：\d{4}.\d{4,5}(v\d+)?。
fn is_arxiv_id(s: &str) -> bool {
    let (base, version) = match s.split_once('v') {
        Some((b, v)) => (b, Some(v)),
        None => (s, None),
    };
    if let Some(v) = version
        && (v.is_empty() || !v.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    let Some((left, right)) = base.split_once('.') else {
        return false;
    };
    left.len() == 4
        && left.chars().all(|c| c.is_ascii_digit())
        && (4..=5).contains(&right.len())
        && right.chars().all(|c| c.is_ascii_digit())
}

/// 解析固定命令；命令词的参数非法时返回 None，让自然语言落入 Agent 入队路径。
pub fn parse_command(content: &str) -> Option<Command> {
    let text = content.trim();
    let lower = text.to_lowercase();
    if matches!(lower.as_str(), "帮助" | "help" | "/help") {
        return Some(Command::Help);
    }
    if matches!(lower.as_str(), "任务" | "tasks" | "/tasks") {
        return Some(Command::List);
    }
    if matches!(lower.as_str(), "水电" | "电费" | "水费" | "utility") {
        return Some(Command::Utility);
    }
    let mut parts = text.splitn(2, char::is_whitespace);
    let verb = parts.next()?.to_lowercase();
    let arg = parts.next().map(str::trim).filter(|s| !s.is_empty());
    match verb.as_str() {
        "任务" | "task" | "show" => Some(Command::Show(arg?.parse().ok()?)),
        "取消" | "cancel" => Some(Command::Cancel(arg?.parse().ok()?)),
        "绑定" | "bind" => Some(Command::Bind(arg?.to_owned())),
        "精读" | "deep" | "deepread" => {
            let id = arg?;
            is_arxiv_id(id).then(|| Command::DeepRead(id.to_owned()))
        }
        _ => None,
    }
}

/// 命令执行上下文：渠道适配器只负责收发，业务判断集中在这里。
pub struct CommandContext<'a> {
    pub store: &'a Arc<dyn TaskStore>,
    /// 未配置口令时绑定功能关闭。
    pub bind_secret: Option<&'a str>,
    /// 当前会话的通知 target（group:{openid} 或 user:{openid}）。
    pub target: &'a str,
    /// 发消息用户的 openid，用于绑定状态判定。
    pub author_openid: &'a str,
    /// 是否单聊；只有单聊且已绑定时才聚合该用户跨会话的任务。
    pub is_c2c: bool,
}

/// 执行命令并返回回复文本；不直接发消息，便于测试。
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
        Command::Help => Ok(HELP_TEXT.to_owned()),
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
        Command::Utility => {
            // 水电余额属个人信息：仅绑定用户在单聊中可查（设计文档 3.10）。
            if !bound {
                return Ok("查询水电需要先绑定身份：发送「绑定 <口令>」".to_owned());
            }
            if !ctx.is_c2c {
                return Ok("水电信息请单聊查询，群聊不展示私人数据".to_owned());
            }
            Ok(format_utility(&latest_reading(ctx.store)?))
        }
        // 精读需要 arXiv 客户端与异步 IO，由 QqBot 在进入 execute 前拦截处理。
        Command::DeepRead(_) => anyhow::bail!("deep read is handled by the bot, not execute"),
    }
}

/// 最近一次水电读数（records 按日期 id 升序，最后一条即最新）。
fn latest_reading(store: &Arc<dyn TaskStore>) -> Result<Option<serde_json::Value>> {
    Ok(store
        .documents("ecard_reading", 30)?
        .into_iter()
        .last()
        .map(|d| d.value))
}

fn format_utility(reading: &Option<serde_json::Value>) -> String {
    let Some(reading) = reading else {
        return "暂无水电读数，请先在服务器运行 ecard poll".to_owned();
    };
    let room = reading["room"].as_str().unwrap_or("宿舍");
    let power = reading["power"].as_f64();
    let water = reading["water"].as_f64();
    let ts = reading["ts"].as_i64().unwrap_or(0);
    let date = crate::util::today_utc(ts);
    let mut lines = vec![room.to_owned()];
    if let Some(p) = power {
        lines.push(format!("电 {p:.2}度"));
    }
    if let Some(w) = water {
        lines.push(format!("水 {w:.2}吨"));
    }
    lines.push(format!(
        "数据时间：{date}（每日自动采集，低余额会主动提醒）"
    ));
    lines.join("\n")
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
