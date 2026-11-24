//! `ru41 course-brush` 的子命令。
//!
//! - `status`：查 sidecar 当前状态（今天该不该刷、目标清单、上一轮结果）
//! - `run`：**强制**跑一轮刷课（不看「今天刷过没」）
//! - `refresh`：让 sidecar 重算目标清单（跑 plan_targets.py）

use std::sync::Arc;

use anyhow::Result;

use crate::client::CourseBrushClient;
use crate::config::CourseBrushSection;

pub const MODULE_NAME: &str = "course-brush";

/// 注册进宿主的 CLI 入口（签名由 `ModuleDef.cli` 定）。
pub fn course_brush_cli(
    config: &ru41::bootstrap::config::Config,
    _store: Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>>
{
    let section = config
        .sources
        .get_or_default::<CourseBrushSection>(MODULE_NAME);
    let args: Vec<String> = args.to_vec();
    Box::pin(ru41::plugins::bridge(async move {
        let value = dispatch(&section, &args).await?;
        Ok(Some(value))
    }))
}

async fn dispatch(section: &CourseBrushSection, args: &[String]) -> Result<serde_json::Value> {
    let command = args
        .first()
        .map(|raw| raw.trim().to_lowercase())
        .unwrap_or_else(|| "status".to_owned());
    let client = CourseBrushClient::new(section).map_err(|error| anyhow::anyhow!("{error}"))?;

    match command.as_str() {
        "status" | "状态" => {
            let status = client
                .status()
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(serde_json::json!({
                "now": status.now_local,
                "busy": status.busy,
                "due": status.due,
                "due_reason": status.due_reason,
                "daily_hour": status.daily_hour,
                "courses": status.courses,
                "last_round": status.last_round,
                "report": status.report,
            }))
        }
        "run" | "刷" => {
            let result = client
                .brush(section.max_minutes(), section.max_courses())
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(serde_json::json!({
                "ok": result.ok,
                "exit_code": result.exit_code,
                "elapsed_secs": result.elapsed_secs,
                "error": result.error,
            }))
        }
        "refresh" | "刷新" => {
            let result = client
                .refresh_targets()
                .await
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(serde_json::json!({
                "ok": result.ok,
                "error": result.error,
            }))
        }
        other => Ok(serde_json::json!({
            "error": format!("未知子命令 {other}（可用：status / run / refresh）")
        })),
    }
}
