//! QQ 消息拦截：识别班会/宣讲会等校园通知，自动加入行程并回执确认。

use crate::extract::extract_notices;
use ru41::app::schedule::ScheduleService;
use ru41::domain::schedule::{EventInput, EventSource, parse_event_when};
use ru41::ports::commands::{CommandContext, CommandHandler, Reply};
use std::{future::Future, pin::Pin};

pub struct NoticeHandler {
    options: ru41::domain::todo::TodoOptions,
}

impl NoticeHandler {
    pub fn new(options: ru41::domain::todo::TodoOptions) -> Self {
        Self { options }
    }
}

impl CommandHandler for NoticeHandler {
    /// 不接命令，只拦自然语言。
    fn matches(&self, _verb: &str) -> bool {
        false
    }

    fn help(&self) -> &'static str {
        "（自动）识别班会/活动通知并加入行程"
    }

    fn handle<'a>(
        &'a self,
        _arg: &'a str,
        _ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            let notices = extract_notices(content);
            if notices.is_empty() {
                return Ok(None);
            }
            let now = ru41::util::now();
            let service = ScheduleService::new(ctx.store.clone(), self.options);
            let mut added = Vec::new();
            let mut skipped = Vec::new();
            for notice in notices {
                let Some((starts_at, ends_at, recurrence)) =
                    parse_event_when(&notice.when_text, now, &self.options)
                else {
                    skipped.push(format!(
                        "{}（时间没看懂：{}）",
                        notice.title, notice.when_text
                    ));
                    continue;
                };
                // 幂等：同名同时刻的行程已存在则跳过（转发重复/多群转发）。
                let dup = ctx
                    .store
                    .documents("event", 10_000)?
                    .iter()
                    .filter_map(|d| {
                        serde_json::from_value::<ru41::domain::schedule::EventItem>(d.value.clone())
                            .ok()
                    })
                    .any(|e| e.title == notice.title && e.starts_at == starts_at);
                if dup {
                    skipped.push(format!("{}（行程里已有）", notice.title));
                    continue;
                }
                service.add(
                    EventInput {
                        title: notice.title.clone(),
                        starts_at,
                        ends_at,
                        location: notice.location.clone(),
                        recurrence,
                        source: Some(EventSource {
                            channel: "qq".into(),
                            source_id: Some(ctx.msg_id.to_owned()),
                        }),
                    },
                    now,
                )?;
                let loc = notice
                    .location
                    .as_deref()
                    .map(|l| format!(" @ {l}"))
                    .unwrap_or_default();
                added.push(format!("- {}｜{}{}", notice.title, notice.when_text, loc));
            }
            if added.is_empty() && skipped.is_empty() {
                return Ok(None);
            }
            let mut text = String::new();
            if !added.is_empty() {
                text.push_str(&format!(
                    "## 📅 已加入行程（{}）\n\n{}",
                    added.len(),
                    added.join("\n")
                ));
            }
            if !skipped.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("\n跳过：{}", skipped.join("；")));
            }
            text.push_str("\n\n> 识别有误可发 `/日程` 查看后取消");
            Ok(Some(Reply::text(text)))
        })
    }
}
