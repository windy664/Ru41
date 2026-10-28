use anyhow::Result;
use ru41::{
    app::task::TaskService,
    domain::records::Document,
    ports::{
        commands::{CommandContext, CommandHandler, Reply, msg_request_key},
        storage::TaskStore,
    },
};
use std::{future::Future, pin::Pin, sync::Arc};

/// QQ 水电查询命令；读数 schema 与权限语义都留在 ecard 模块内。
pub struct EcardQqCommand {
    backend: String,
}

impl EcardQqCommand {
    pub fn new(backend: String) -> Self {
        Self { backend }
    }
}

impl CommandHandler for EcardQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "水电" | "电费" | "水费" | "utility")
    }

    fn help(&self) -> &'static str {
        "`/水电`：查询宿舍水电余额（需绑定身份，仅单聊）；也可直接问“电费还剩多少”"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !arg.is_empty() {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("查询水电", "水电属于个人信息")? {
                return Ok(Some(Reply::text(reply)));
            }
            Ok(Some(Reply::text(format_utility(
                latest_reading(ctx.store)?.as_ref(),
            ))))
        })
    }

    fn handle_message<'a>(
        &'a self,
        content: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Reply>>> + Send + 'a>> {
        Box::pin(async move {
            if !looks_like_utility_question(content) {
                return Ok(None);
            }
            if let Some(reply) = ctx.require_private("查询水电", "水电属于个人信息")? {
                return Ok(Some(Reply::text(reply)));
            }
            let history = recent_readings(ctx.store, 30)?;
            let Some(latest) = history.last() else {
                return Ok(Some(Reply::text(
                    "## 暂无水电读数\n\n请先在服务器运行 `ecard poll`。".to_owned(),
                )));
            };
            let readings: Vec<serde_json::Value> = history
                .iter()
                .map(|document| {
                    serde_json::json!({
                        "date": document.id,
                        "power": document.value.get("power").cloned().unwrap_or(serde_json::Value::Null),
                        "water": document.value.get("water").cloned().unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            let prompt = analysis_prompt(content, &readings);
            let queued = TaskService::new(ctx.store.clone()).enqueue_with_notify(
                &format!("{}:ecard-analysis", msg_request_key(ctx.msg_id)),
                &prompt,
                &self.backend,
                &ctx.reply_route(),
            )?;
            let status = if queued.created {
                format!("已生成分析任务 `#{}`", queued.task.id)
            } else {
                format!("该问题已在任务 `#{}` 中分析", queued.task.id)
            };
            Ok(Some(Reply::text(format!(
                "## 正在分析水电\n\n已读取最近 {} 条记录，最新数据时间为 **{}**。\n\n> {status}，完成后会回复当前会话。",
                readings.len(),
                latest.id
            ))))
        })
    }
}

fn looks_like_utility_question(content: &str) -> bool {
    let text = content.trim().to_lowercase();
    let mentions_utility = [
        "水电",
        "电费",
        "水费",
        "电量",
        "水量",
        "用电",
        "用水",
        "多少电",
        "多少水",
        "还剩多少电",
        "还剩多少水",
    ]
    .iter()
    .any(|word| text.contains(word));
    let asks_for_result = [
        "多少",
        "还剩",
        "余额",
        "分析",
        "趋势",
        "用了",
        "消耗",
        "够用",
        "充值",
        "查询",
        "看看",
        "看一下",
        "查一下",
        "那些",
        "怎么样",
        "咋样",
        "近期",
        "最近",
        "这几天",
        "近几天",
        "这段时间",
        "本月",
        "这个月",
    ]
    .iter()
    .any(|word| text.contains(word));
    mentions_utility && asks_for_result
}

fn analysis_prompt(question: &str, readings: &[serde_json::Value]) -> String {
    format!(
        "用户提出宿舍水电问题：\n{question}\n\n\
        以下数据已经由 QQ 的 ecard 模块完成身份绑定和单聊校验，按日期升序排列，最多包含最近 30 条：\n\
        ```json\n{}\n```\n\n\
        请使用 ecard-utility 技能回答：先给出最新电、水余额和数据时间，再根据用户提供的问题分析变化。\
        余额下降只能作为用量近似；余额上升可能来自充值。只有一个读数时不要推断趋势或预计可用天数。\
        不要把缓存读数说成实时查询，不要编造阈值、房间、账号或缺失字段。",
        serde_json::to_string_pretty(readings).unwrap_or_else(|_| "[]".to_owned())
    )
}

/// 最近读数；records 按日期 id 升序，截取末尾以避免历史超过 limit 后读到旧数据。
fn recent_readings(store: &Arc<dyn TaskStore>, limit: usize) -> Result<Vec<Document>> {
    let mut documents = store.documents("ecard_reading", 10_000)?;
    let start = documents.len().saturating_sub(limit);
    documents.drain(..start);
    Ok(documents)
}

/// 最近一次水电读数。
fn latest_reading(store: &Arc<dyn TaskStore>) -> Result<Option<Document>> {
    Ok(recent_readings(store, 1)?.into_iter().last())
}

fn format_utility(reading: Option<&Document>) -> String {
    let Some(reading) = reading else {
        return "## 暂无水电读数\n\n请先在服务器运行 `ecard poll`。".to_owned();
    };
    let room = reading.value["room"].as_str().unwrap_or("宿舍");
    let power = reading.value["power"].as_f64();
    let water = reading.value["water"].as_f64();
    let mut lines = vec![format!("## {room} 水电")];
    if let Some(p) = power {
        lines.push(format!("- **电**：{p:.2}度"));
    }
    if let Some(w) = water {
        lines.push(format!("- **水**：{w:.2}吨"));
    }
    lines.push(format!("- **数据时间**：{}", reading.id));
    lines.push("\n> 每日自动采集，低余额会主动提醒。".to_owned());
    lines.join("\n")
}
