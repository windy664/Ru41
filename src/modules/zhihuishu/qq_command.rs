use crate::ports::commands::{CommandContext, CommandHandler, Reply};
use anyhow::Result;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use super::source::{load_report, parse_platform_time};

/// 临近截止窗口（天）；与推送提醒的默认 deadline_days 对齐。
const DUE_SOON_DAYS: i64 = 3;

/// QQ 作业查询命令；报告契约与权限语义都留在 zhihuishu 模块内。
/// 只读契约文件，不写 records，不触碰平台协议。
pub struct ZhihuishuQqCommand {
    report_path: PathBuf,
}

impl ZhihuishuQqCommand {
    pub fn new(report_path: PathBuf) -> Self {
        Self { report_path }
    }
}

impl CommandHandler for ZhihuishuQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "作业" | "homework" | "deadline")
    }

    fn help(&self) -> &'static str {
        "`/作业`：查询智慧树未完成作业与截止时间（需绑定身份，仅单聊）"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            // 带参数的「/作业 xxx」不拦截：避免误吞自然语言, 交给 Agent。
            if !arg.is_empty() {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("查询作业", "作业信息属于个人数据")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            Ok(Some(Reply::text(format_homework(&self.report_path))))
        })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !looks_like_homework_question(content) {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("查询作业", "作业信息属于个人数据")?
            {
                return Ok(Some(Reply::text(reply)));
            }
            Ok(Some(Reply::text(format_homework(&self.report_path))))
        })
    }
}

/// 自然语言命中要克制：既提「作业/截止」又带疑问色彩才拦截，避免日常闲聊被吞。
/// 两道保险：只拦截短问句（长文一律交给 Agent），且「查」必须带宾语（裸「查」
/// 会命中「筛查/检查/调查」等，比赛公告、新闻长文常被误吞）。
fn looks_like_homework_question(content: &str) -> bool {
    let text = content.trim().to_lowercase();
    if text.chars().count() > 80 {
        return false;
    }
    let topic = ["作业", "截止", "deadline", "due"]
        .iter()
        .any(|k| text.contains(k));
    let asking = [
        "什么",
        "哪些",
        "还有",
        "有没有",
        "查下",
        "查询",
        "查查",
        "快",
        "？",
        "?",
    ]
    .iter()
    .any(|k| text.contains(k));
    topic && asking
}

/// 读契约报告并排版；报告缺失/损坏时友好降级而不是报错打断会话。
fn format_homework(path: &Path) -> String {
    let report = match load_report(path) {
        Ok(report) => report,
        Err(_) => {
            return "## 报告暂不可用\n\n采集器还没有产出报告，等下一轮采集后再问吧。".to_owned();
        }
    };

    if report.login_ok == Some(false) {
        return "## 登录态失效\n\n智慧树 cookies 过期且自动重登未成功，需要人工在 sidecar 里重新登录，恢复前清单可能过期。".to_owned();
    }

    let now = crate::util::now();
    let urgent = DUE_SOON_DAYS * 86_400;
    let mut overdue: Vec<(i64, String, bool)> = Vec::new();
    let mut due_soon: Vec<(i64, String, bool)> = Vec::new();
    let mut later: Vec<(i64, String, bool)> = Vec::new();
    for course in &report.courses {
        for task in &course.tasks {
            if task.completed.unwrap_or(false) {
                continue;
            }
            let label = format!("{}｜{}", course.title, task.title);
            let exam =
                task.exam_duration_minutes.unwrap_or(0) > 0 || task.video_invigilate == Some(true);
            let label = if exam {
                format!("🔒{label}（需人工完成）")
            } else {
                label
            };
            let end = task.end_at.as_deref().and_then(parse_platform_time);
            match end {
                Some(ts) if ts < now => overdue.push((ts, label, exam)),
                Some(ts) if ts - now <= urgent => due_soon.push((ts, label, exam)),
                Some(ts) => later.push((ts, label, exam)),
                None => later.push((i64::MAX, format!("{label}（截止未知）"), exam)),
            }
        }
    }

    if overdue.is_empty() && due_soon.is_empty() && later.is_empty() {
        return "## 作业清单\n\n当前没有未完成作业 🎉".to_owned();
    }

    let mut lines = vec!["## 作业清单".to_owned()];
    let mut section = |title: &str, mut items: Vec<(i64, String, bool)>| {
        if items.is_empty() {
            return;
        }
        items.sort_by_key(|(ts, _, _)| *ts);
        lines.push(format!("\n**{title}**"));
        for (_, label, _) in items {
            lines.push(format!("- {label}"));
        }
    };
    section("已过期", overdue);
    section(&format!("{DUE_SOON_DAYS} 天内截止"), due_soon);
    section("稍后", later);
    lines.join("\n")
}
