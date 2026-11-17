//! 校园跑的专属 CLI：
//!
//! ```text
//! ru41 campusrun status          学期进度
//! ru41 campusrun records [条数]   最近记录
//! ru41 campusrun cheat           违规名单状态
//! ru41 campusrun all             上面三样一起
//! ru41 campusrun rules           规则面：窗口/必过点/围栏/单次门槛
//! ru41 campusrun plan --laps 3   预演（不提交）
//! ru41 campusrun plan --km 1.2   预演（不提交）
//! ru41 campusrun submit --laps 3 登记一条待确认的提交（仍不提交）
//! ru41 campusrun confirm <token> 确认并提交（写操作！）
//! ```
//!
//! 输出是 Markdown（宿主统一 `to_string_pretty` 打出来）。

use crate::client::CampusrunClient;
use crate::config::CampusrunSection;
use crate::service::fetch_markdown;
use crate::submit::{RunIntent, make_token};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// 命令名（宿主按模块名路由到本钩子）。
pub const MODULE_NAME: &str = "campusrun";

/// 宿主入口：`ru41 campusrun <子命令>`。
pub fn campusrun_cli(
    config: &ru41::bootstrap::config::Config,
    store: Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<serde_json::Value>>> + Send>>
{
    let section = config
        .sources
        .get_or_default::<CampusrunSection>(MODULE_NAME);
    let args: Vec<String> = args.to_vec();
    Box::pin(ru41::plugins::bridge(async move {
        let value = dispatch(&section, &store, &args).await?;
        Ok(Some(value))
    }))
}

/// 分发子命令。
async fn dispatch(
    section: &CampusrunSection,
    store: &Arc<dyn ru41::ports::storage::TaskStore>,
    args: &[String],
) -> Result<serde_json::Value> {
    let command = args
        .first()
        .map(|raw| raw.trim().to_lowercase())
        .unwrap_or_else(|| "all".to_owned());
    let flags = parse_flags(&args[1..]);
    match command.as_str() {
        "status" | "进度" => {
            let client = client(section)?;
            Ok(text(fetch_markdown(&client, "status", 5).await?))
        }
        "records" | "记录" => {
            let limit = flags
                .get("n")
                .and_then(|raw| raw.parse::<usize>().ok())
                .unwrap_or(5);
            let client = client(section)?;
            Ok(text(fetch_markdown(&client, "records", limit).await?))
        }
        "cheat" | "风控" | "状态" => {
            let client = client(section)?;
            Ok(text(fetch_markdown(&client, "cheat", 5).await?))
        }
        "all" | "全部" => {
            let client = client(section)?;
            Ok(text(fetch_markdown(&client, "all", 5).await?))
        }
        "rules" | "规则" => {
            let client = client(section)?;
            Ok(text(crate::service::fetch_rules(&client).await?))
        }
        "plan" | "预演" => {
            let client = client(section)?;
            let intent = intent_from_flags(&flags)?;
            let (summary, plan, _params) =
                crate::service::fetch_plan(&client, section, &intent).await?;
            let blocked = plan.blocked.unwrap_or(false);
            Ok(text(format!(
                "{summary}\n\n{}",
                if blocked {
                    "预演被拦下，**不能提交**。"
                } else {
                    "预演通过，可以提交。"
                }
            )))
        }
        // ⚠️ 不能叫 "run"：宿主把 `ru41 <模块> run` 当成 tracker 的长驻 run
        // 抢走（见 src/main.rs 的 Tracker 分发），根本进不到插件的 cli 钩子。
        "submit" | "跑" | "提交" => {
            // 只登记，不提交：提交要用户再确认一次（confirm）。
            let client = client(section)?;
            let intent = intent_from_flags(&flags)?;
            let (summary, plan, params) =
                crate::service::fetch_plan(&client, section, &intent).await?;
            if plan.blocked.unwrap_or(false) {
                anyhow::bail!(
                    "预演自检没过，不能提交：{}",
                    plan.issue_summary.unwrap_or_default()
                );
            }
            if !section.allow_submit() {
                anyhow::bail!(
                    "提交功能没开：[sources.campusrun] 里 allow_submit = true 才会真提交。\n\
                     （刚才只做了预演，没有提交任何记录。）"
                );
            }
            let now = ru41::util::now();
            let token = make_token(&args.join(" "), now);
            crate::service::register_pending(store, &params, &summary, &token, now)?;
            Ok(text(format!(
                "{summary}\n\n\
                 还没有提交。确认要提交就执行：\n\
                 `ru41 campusrun confirm {token}`\n\
                 （待确认有效期 {} 分钟；提交后 {} 秒内不能再跑）",
                section.confirm_ttl_secs() / 60,
                section.silence_secs()
            )))
        }
        "confirm" | "确认" => {
            let token = args
                .get(1)
                .map(|raw| raw.trim().trim_matches('`').to_owned())
                .filter(|raw| !raw.is_empty())
                .context("用法: ru41 campusrun confirm <token>")?;
            let client = client(section)?;
            let now = ru41::util::now();
            let outcome =
                crate::service::confirm_and_submit(store, &client, section, &token, now).await?;
            Ok(text(outcome.text()))
        }
        other => Err(anyhow::anyhow!("{other} 不是已知子命令。\n{}", usage())),
    }
}

/// CLI 用法文案。
pub fn usage() -> String {
    "用法:\n  \
     ru41 campusrun status            学期进度\n  \
     ru41 campusrun records [条数]     最近记录\n  \
     ru41 campusrun cheat             违规名单状态\n  \
     ru41 campusrun all               进度+记录+状态\n  \
     ru41 campusrun rules             规则面（窗口/必过点/围栏/门槛）\n  \
     ru41 campusrun plan --laps 3     预演（不提交）\n  \
     ru41 campusrun plan --km 1.2     预演（不提交）\n  \
     ru41 campusrun submit --laps 3    登记待确认提交（不提交）\n  \
     ru41 campusrun submit --km 1.2    同上\n  \
     ru41 campusrun confirm <token>   确认并提交（写操作）"
        .to_owned()
}

/// 构造客户端。
fn client(section: &CampusrunSection) -> Result<CampusrunClient> {
    CampusrunClient::new(section).map_err(|error| anyhow::anyhow!("{error}"))
}

/// Markdown → JSON 字符串（宿主按 JSON 打出来）。
fn text(body: String) -> serde_json::Value {
    serde_json::Value::String(body)
}

/// 解析 `--key value` 形式的参数。
fn parse_flags(args: &[String]) -> HashMap<String, String> {
    let mut flags = HashMap::new();
    let mut index = 0;
    while index < args.len() {
        let raw = args[index].trim();
        if let Some(key) = raw.strip_prefix("--") {
            let value = args
                .get(index + 1)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.starts_with("--"))
                .unwrap_or_default();
            if !value.is_empty() {
                flags.insert(key.to_lowercase(), value);
                index += 2;
                continue;
            }
            flags.insert(key.to_lowercase(), "1".to_owned());
        }
        index += 1;
    }
    flags
}

/// 从参数里读出「跑什么」。
fn intent_from_flags(flags: &HashMap<String, String>) -> Result<RunIntent> {
    if let Some(raw) = flags.get("laps").or_else(|| flags.get("圈")) {
        let laps: f64 = raw.parse().with_context(|| format!("圈数不对：{raw}"))?;
        let lane = flags
            .get("lane")
            .or_else(|| flags.get("道"))
            .and_then(|raw| raw.parse::<u8>().ok())
            .unwrap_or(1);
        return Ok(RunIntent::Laps { laps, lane });
    }
    if let Some(raw) = flags.get("km").or_else(|| flags.get("公里")) {
        let km: f64 = raw.parse().with_context(|| format!("公里数不对：{raw}"))?;
        return Ok(RunIntent::DistanceKm { km });
    }
    anyhow::bail!("要指定跑多少：--laps 3（圈）或 --km 1.2（公里）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        routing::{get, post},
    };
    use serde_json::json;
    use tokio::sync::Mutex;

    /// mock 控制台：记录收到的路径与请求体，按路径返回固定数据。
    ///
    /// ⚠️ 这里**不会**打到真的 pace-forge：提交路径只在 mock 上验证。
    async fn mock_console() -> (String, Arc<Mutex<Vec<(String, serde_json::Value)>>>) {
        let seen: Arc<Mutex<Vec<(String, serde_json::Value)>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let plan_recorder = recorder.clone();
        let app = Router::new()
            .route(
                "/api/starttimes",
                get(|| async {
                    Json(json!({
                        "ok": true,
                        "data": {
                            "mode": "now",
                            "startMs": 1_794_890_000_000i64,
                            "stopMs": 1_794_890_720_000i64,
                            "startText": "2026-11-17 07:00:00",
                            "stopText": "2026-11-17 07:12:00",
                            "durationSec": 720,
                            "limits": {
                                "maxDaysAgo": 3,
                                "maxAgoMin": 4320,
                                "randHourLo": 7,
                                "randHourHi": 20
                            }
                        }
                    }))
                }),
            )
            .route(
                "/api/records",
                get(|| async {
                    Json(json!({
                        "ok": true,
                        "data": {"records": [{
                            "rrid": 1367213418,
                            "startTime": 1_794_870_446_000i64,
                            "totalDis": 1212.0,
                            "totalTime": 426.0,
                            "validDis": 1212.0,
                            "complete": true,
                            "status": 0,
                            "appealStatus": 0,
                            "stepFreqStatus": 0,
                            "avgStepFreq": 158
                        }]}
                    }))
                }),
            )
            .route(
                "/api/record/1367213418",
                get(|| async {
                    Json(json!({
                        "ok": true,
                        "data": {
                            "rrid": 1367213418,
                            "selDistance": 1.0,
                            "speedTop": 2.17,
                            "speedBottom": 10.0,
                            "statusInfo": "太棒了，恭喜您完成了跑步",
                            "reasonList": [
                                {"complete": true, "completeStatus": 1, "type": 9, "reason": "按顺序通过所有点位"},
                                {"complete": true, "completeStatus": 1, "type": 1, "reason": "最少要跑1.00公里"},
                                {"complete": true, "completeStatus": 1, "type": 7, "reason": "跑7分钟以上"}
                            ]
                        }
                    }))
                }),
            )
            .route(
                "/api/plan",
                post(move |body: axum::Json<serde_json::Value>| {
                    let recorder = plan_recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .await
                            .push(("/api/plan".to_owned(), body.0.clone()));
                        Json(json!({
                            "ok": true,
                            "data": {
                                "online": true,
                                "distance": 1.2,
                                "duration": 7.26,
                                "actualLength": 1200,
                                "steps": 1906,
                                "avgCadence": 158.8,
                                "pointCount": 407,
                                "checkpointTotal": 5,
                                "checkpointPassed": 5,
                                "blocked": false,
                                "issueSummary": "全部通过",
                                "trackMode": "stadium",
                                "policy": {"minDistance": 1000},
                                "logs": ["√ [preflight] 全部通过"],
                                "note": "未提交"
                            }
                        }))
                    }
                }),
            )
            .route(
                "/api/run",
                post(move |body: axum::Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    async move {
                        recorder
                            .lock()
                            .await
                            .push(("/api/run".to_owned(), body.0.clone()));
                        Json(json!({
                            "ok": true,
                            "data": {"recordId": 1367213418, "submitted": true, "blocked": false}
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), seen)
    }

    fn section_with(base_url: &str, allow_submit: bool) -> CampusrunSection {
        CampusrunSection {
            base_url: Some(base_url.to_owned()),
            token: Some("secret-token".into()),
            allow_submit: Some(allow_submit),
            ..Default::default()
        }
    }

    async fn store() -> Arc<dyn ru41::ports::storage::TaskStore> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state.db");
        // tmpdir 会在函数结束后被删，但测试期间够用。
        let store: Arc<dyn ru41::ports::storage::TaskStore> =
            Arc::new(ru41::adapters::database::sqlite::SqliteTaskStore::open(&path).unwrap());
        std::mem::forget(tmp);
        store
    }

    #[tokio::test]
    async fn plan_never_submits() {
        let (base_url, seen) = mock_console().await;
        let section = section_with(&base_url, true);
        let store = store().await;
        let args = vec!["plan".to_owned(), "--laps".to_owned(), "3".to_owned()];
        let value = dispatch(&section, &store, &args).await.unwrap();
        let text = value.as_str().unwrap();
        assert!(text.contains("校园跑预演"), "{text}");
        assert!(text.contains("1.20 km"), "{text}");
        assert!(text.contains("预演通过"), "{text}");
        // 只有 /api/plan，**没有** /api/run。
        let calls = seen.lock().await;
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(calls[0].0, "/api/plan");
        // 请求体是校园跑参数。
        let body = &calls[0].1;
        assert_eq!(body["runMode"], 1);
        assert_eq!(body["sportType"], 1);
        assert_eq!(body["mode"], "free");
        assert!((body["distance"].as_f64().unwrap() - 1.2).abs() < 0.01);
    }

    /// 距离门槛用**服务端 policy.minDistance**（mock 返回 1000 m）：
    /// 1 圈 = 0.4 km → 拒，并说清最少 3 圈。
    #[tokio::test]
    async fn plan_below_server_min_distance_is_refused() {
        let (base_url, seen) = mock_console().await;
        let section = section_with(&base_url, true);
        let store = store().await;
        let args = vec!["plan".to_owned(), "--laps".to_owned(), "1".to_owned()];
        let error = dispatch(&section, &store, &args)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("1.00 km"), "{error}");
        assert!(error.contains("3 圈"), "要说清最少几圈：{error}");
        let calls = seen.lock().await;
        assert!(
            calls.iter().all(|(path, _)| path == "/api/plan"),
            "不该有提交请求：{calls:?}"
        );
    }

    #[tokio::test]
    async fn run_refuses_when_submit_is_disabled() {
        let (base_url, seen) = mock_console().await;
        let section = section_with(&base_url, false);
        let store = store().await;
        let args = vec!["submit".to_owned(), "--laps".to_owned(), "3".to_owned()];
        let error = dispatch(&section, &store, &args)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("allow_submit"), "{error}");
        // 只做了预演，没有提交。
        let calls = seen.lock().await;
        assert!(
            calls.iter().all(|(path, _)| path == "/api/plan"),
            "不该有提交请求：{calls:?}"
        );
    }

    #[tokio::test]
    async fn run_then_confirm_submits_campusrun_params() {
        let (base_url, seen) = mock_console().await;
        let section = section_with(&base_url, true);
        let store = store().await;
        let args = vec!["submit".to_owned(), "--laps".to_owned(), "3".to_owned()];
        let value = dispatch(&section, &store, &args).await.unwrap();
        let text = value.as_str().unwrap().to_owned();
        assert!(text.contains("还没有提交"), "{text}");
        // 从回执里抠出口令。
        let token = text
            .split("confirm ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|token| token.trim_matches('`').to_owned())
            .expect("回执里应有确认口令");

        // 登记之后、确认之前：没有提交请求。
        {
            let calls = seen.lock().await;
            assert!(
                calls.iter().all(|(path, _)| path == "/api/plan"),
                "确认之前不该提交：{calls:?}"
            );
        }

        let confirm = vec!["confirm".to_owned(), token];
        let value = dispatch(&section, &store, &confirm).await.unwrap();
        let text = value.as_str().unwrap();
        assert!(text.contains("已提交"), "{text}");
        assert!(text.contains("1367213418"), "{text}");

        let calls = seen.lock().await;
        let submits: Vec<_> = calls
            .iter()
            .filter(|(path, _)| path == "/api/run")
            .collect();
        assert_eq!(submits.len(), 1, "只应提交一次：{calls:?}");
        let body = &submits[0].1;
        assert_eq!(body["runMode"], 1, "必须是记分跑");
        assert_eq!(body["sportType"], 1);
        assert!((body["distance"].as_f64().unwrap() - 1.2).abs() < 0.01);
    }

    #[tokio::test]
    async fn confirm_cannot_be_replayed() {
        let (base_url, seen) = mock_console().await;
        let section = section_with(&base_url, true);
        let store = store().await;
        let args = vec!["submit".to_owned(), "--laps".to_owned(), "3".to_owned()];
        let text = dispatch(&section, &store, &args)
            .await
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let token = text
            .split("confirm ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|token| token.trim_matches('`').to_owned())
            .unwrap();
        let confirm = vec!["confirm".to_owned(), token.clone()];
        dispatch(&section, &store, &confirm).await.unwrap();
        // 同一口令再确认一次 → 拒绝，不能提交第二条。
        let error = dispatch(&section, &store, &confirm)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("不能重复提交"), "{error}");
        let calls = seen.lock().await;
        assert_eq!(
            calls.iter().filter(|(path, _)| path == "/api/run").count(),
            1,
            "同一口令只能提交一次：{calls:?}"
        );
    }
}
