//! 自动刷课附属的集成测试：拿一个本地 mock sidecar 验证
//! 请求路径、请求体、响应解析（尤其是**缺字段/多字段**不能炸）与报告渲染。
//!
//! 真机形态：sidecar 是 `ru41-sidecar` 容器，`/status`、`/brush`、`/targets/refresh`
//! 都是 JSON；`/brush` 是同步的长任务（浏览器刷课）。

use axum::{Router, routing::get, routing::post};
use ru41_plugin_course_brush::{CourseBrushClient, CourseBrushSection};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// mock sidecar：记录收到的请求体，按路径返回固定数据。
async fn mock_sidecar() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();

    // 每个 handler 各自持一份 recorder(闭包是 move 的, 不能共用同一个变量)
    let seen_status = recorder.clone();
    let seen_brush = recorder.clone();

    let app = Router::new()
        .route(
            "/status",
            get(move || {
                let recorder = seen_status.clone();
                async move {
                    recorder.lock().await.push(json!({"path": "/status"}));
                    axum::Json(json!({
                        "ok": true,
                        "busy": false,
                        "now_local": "2026-11-24 19:05:00",
                        "timezone": "UTC+08:00",
                        "due": true,
                        "due_reason": "还没刷过，今天该刷",
                        "daily_hour": 19,
                        "generated_at": "2026-11-23 16:02:28",
                        "targets_generated_at": "2026-11-22 20:35:49",
                        "courses": [
                            {"title": "程序设计基础", "engine": "aismart", "progress": 2,
                             "url": "https://example.invalid/a"},
                            {"title": "国家安全教育", "engine": "polymas", "progress": 60,
                             "url": "https://example.invalid/b"}
                        ],
                        "last_round": {
                            "generated_at": "2026-11-23 16:02:28",
                            "results": [
                                {"title": "course-home", "watched": 27, "failed": 0, "error": null}
                            ]
                        },
                        "report": {
                            "generated_at": "2026-11-23 20:34:50",
                            "login_ok": true,
                            "courses": 10,
                            "tasks": 17,
                            "content_ready": ["程序设计基础", "国家安全教育"]
                        }
                    }))
                }
            }),
        )
        .route(
            "/health",
            get(|| async { axum::Json(json!({"ok": true, "busy": false})) }),
        )
        .route(
            "/brush",
            post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let recorder = seen_brush.clone();
                async move {
                    recorder.lock().await.push(body.clone());
                    axum::Json(json!({
                        "ok": true,
                        "exit_code": 0,
                        "elapsed_secs": 1830,
                        "status": {
                            "ok": true,
                            "now_local": "2026-11-24 21:00:00",
                            "due": false,
                            "due_reason": "今天已经刷过一轮",
                            "courses": [],
                            "last_round": {
                                "generated_at": "2026-11-24 13:00:00",
                                "results": [
                                    {"title": "程序设计基础", "watched": 18, "failed": 0, "error": null}
                                ]
                            },
                            "report": {"content_ready": ["程序设计基础"]}
                        }
                    }))
                }
            }),
        )
        .route(
            "/targets/refresh",
            post(|| async { axum::Json(json!({"ok": true})) }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), seen)
}

fn section(base_url: &str) -> CourseBrushSection {
    CourseBrushSection {
        base_url: Some(base_url.to_owned()),
        timeout_secs: Some(5),
        brush_timeout_secs: Some(30),
        ..Default::default()
    }
}

#[tokio::test]
async fn status_parses_full_payload() {
    let (base, _seen) = mock_sidecar().await;
    let client = CourseBrushClient::new(&section(&base)).unwrap();
    let status = client.status().await.unwrap();

    assert!(status.due, "该刷的场合 due 要为 true");
    assert_eq!(status.due_reason.as_deref(), Some("还没刷过，今天该刷"));
    assert_eq!(status.courses.len(), 2);
    assert_eq!(status.courses[0].title.as_deref(), Some("程序设计基础"));
    assert_eq!(status.courses[1].engine.as_deref(), Some("polymas"));
    assert_eq!(status.last_round.results.len(), 1);
    assert_eq!(status.last_round.results[0].watched, Some(27));
    assert_eq!(status.report.content_ready.len(), 2);
    assert!(client.health().await.unwrap());
}

#[tokio::test]
async fn status_tolerates_missing_fields() {
    // sidecar 升级前后的字段差异不能让插件炸 —— 缺字段一律走默认值。
    let app = Router::new().route(
        "/status",
        get(|| async { axum::Json(json!({"ok": true, "due": false, "unknown_field": 42})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CourseBrushClient::new(&section(&format!("http://{addr}"))).unwrap();
    let status = client.status().await.unwrap();
    assert!(!status.due);
    assert!(status.courses.is_empty());
    assert!(status.report.content_ready.is_empty());
    assert!(status.last_round.results.is_empty());
}

#[tokio::test]
async fn brush_sends_budget_in_body() {
    let (base, seen) = mock_sidecar().await;
    let client = CourseBrushClient::new(&section(&base)).unwrap();
    let result = client.brush(120, 3).await.unwrap();

    assert!(result.ok);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.elapsed_secs, Some(1830));

    let seen = seen.lock().await;
    let body = seen
        .iter()
        .find(|item| item.get("total_minutes").is_some())
        .expect("应该发过 /brush 请求体");
    assert_eq!(body["total_minutes"], json!(120));
    assert_eq!(body["max_courses"], json!(3));
}

#[tokio::test]
async fn config_defaults_and_overrides() {
    let default = CourseBrushSection::default();
    assert_eq!(default.base_url(), "http://ru41-sidecar:9100");
    assert_eq!(default.max_minutes(), 120);
    assert_eq!(default.max_courses(), 3);
    assert!(default.notify(), "默认该发通知");
    assert!(default.is_ready());

    let custom = CourseBrushSection {
        base_url: Some("http://example.invalid:9100/".to_owned()),
        max_minutes: Some(45),
        max_courses: Some(1),
        notify: Some(false),
        ..Default::default()
    };
    assert_eq!(
        custom.base_url(),
        "http://example.invalid:9100",
        "尾部斜杠要去掉"
    );
    assert_eq!(custom.max_minutes(), 45);
    assert_eq!(custom.max_courses(), 1);
    assert!(!custom.notify());
}

#[test]
fn config_rejects_unknown_keys() {
    // 配置写错字段要当场报错，别静默忽略（deny_unknown_fields）。
    let parsed: Result<CourseBrushSection, _> =
        toml::from_str("base_url = \"x\"\ntypo_field = 1\n");
    assert!(parsed.is_err(), "未知字段应当报错");
}
