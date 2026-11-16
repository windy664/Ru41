//! 校园跑的专属 CLI：`ru41 campusrun status|records|cheat|all`。
//!
//! 输出是 Markdown（agent skill 直接读，不用再解析 JSON）。

use crate::client::CampusrunClient;
use crate::config::CampusrunSection;
use crate::service::fetch_markdown;
use anyhow::Result;
use std::sync::Arc;

/// 命令名（宿主按模块名路由到本钩子）。
pub const MODULE_NAME: &str = "campusrun";

/// 宿主入口：`ru41 campusrun <子命令>`。
pub fn campusrun_cli(
    config: &ru41::bootstrap::config::Config,
    _store: Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>>
{
    let section = config
        .sources
        .get_or_default::<CampusrunSection>(MODULE_NAME);
    let what = args
        .first()
        .map(|raw| raw.trim().to_lowercase())
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| "all".to_owned());
    let limit = args
        .get(1)
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(5);
    Box::pin(ru41::plugins::bridge(async move {
        let client = CampusrunClient::new(&section).map_err(|error| anyhow::anyhow!("{error}"))?;
        let text = fetch_markdown(&client, &what, limit)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        Ok(Some(serde_json::Value::String(text)))
    }))
}

/// CLI 用法文案。
pub fn usage() -> String {
    "用法: ru41 campusrun status | records [条数] | cheat | all".to_owned()
}
