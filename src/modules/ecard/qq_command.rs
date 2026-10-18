use crate::{
    app::identity::IdentityService,
    channels::qq::commands::{CommandContext, QqCommandHandler},
    ports::storage::TaskStore,
};
use anyhow::Result;
use std::{future::Future, pin::Pin, sync::Arc};

/// QQ 水电查询命令；读数 schema 与权限语义都留在 ecard 模块内。
pub struct EcardQqCommand;

impl QqCommandHandler for EcardQqCommand {
    fn matches(&self, verb: &str) -> bool {
        matches!(verb, "水电" | "电费" | "水费" | "utility")
    }

    fn help(&self) -> &'static str {
        "水电 - 查询宿舍水电余额（需绑定身份，仅单聊）"
    }

    fn handle<'a>(
        &'a self,
        arg: &'a str,
        ctx: &'a CommandContext<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + Send + 'a>> {
        Box::pin(async move {
            if !arg.is_empty() {
                return Ok(None);
            }
            // 水电余额属个人信息：仅绑定用户在单聊中可查（设计文档 3.10）。
            let identity = IdentityService::new(ctx.store.clone());
            if !identity.is_bound("qq", ctx.author_openid)? {
                return Ok(Some(
                    "查询水电需要先绑定身份：发送「绑定 <口令>」".to_owned(),
                ));
            }
            if !ctx.is_c2c {
                return Ok(Some("水电信息请单聊查询，群聊不展示私人数据".to_owned()));
            }
            Ok(Some(format_utility(&latest_reading(ctx.store)?)))
        })
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
