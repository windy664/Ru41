use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use ru41::{
    adapters::{agent::codex::CodexAgent, database::sqlite::SqliteTaskStore},
    ingress::http::{ApiState, router},
    ports::{agent::BackendRegistry, storage::TaskStore},
};
use std::sync::Arc;
use tower::ServiceExt;

fn setup() -> (router_type::Router, Arc<SqliteTaskStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    std::mem::forget(dir);
    let mut backends = BackendRegistry::default();
    backends.register(CodexAgent::default()).unwrap();
    let state = ApiState::new(
        store.clone() as Arc<dyn TaskStore>,
        Arc::new(backends),
        "test-token-0123456789abcdef".into(),
    )
    .unwrap();
    (router(state), store)
}
mod router_type {
    pub type Router = axum::Router;
}

async fn get(router: &router_type::Router, path: &str) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, "Bearer test-token-0123456789abcdef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 128 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn requests_without_valid_token_are_rejected() {
    let (router, _) = setup();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn task_lifecycle_over_http() {
    let (router, store) = setup();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tasks")
                .header(header::AUTHORIZATION, "Bearer test-token-0123456789abcdef")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"key":"api:e1","prompt":"你好","backend":"codex"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], "queued");

    let (status, body) = get(&router, "/api/v1/tasks?limit=5").await;
    assert_eq!(status, StatusCode::OK);
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);

    let id = body_list_id(&list);
    let (status, body) = get(&router, &format!("/api/v1/tasks/{id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("queued"));
    // 存储侧同样可见：HTTP 与 CLI/QQ 共用同一应用服务。
    assert!(store.get(id).unwrap().is_some());

    // 未知底座被拒绝。
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tasks")
                .header(header::AUTHORIZATION, "Bearer test-token-0123456789abcdef")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"key":"api:e2","prompt":"x","backend":"nope"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

fn body_list_id(list: &serde_json::Value) -> i64 {
    list[0]["id"].as_i64().unwrap()
}

#[tokio::test]
async fn backends_and_openapi_are_served() {
    let (router, _) = setup();
    let (status, body) = get(&router, "/api/v1/backends").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("codex"));
    let (status, body) = get(&router, "/api/v1/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    let spec: serde_json::Value = serde_json::from_str(&body).unwrap();
    // 契约必须覆盖全部核心路由与 schema（schema 由 schemars 从类型生成）。
    for path in [
        "/tasks",
        "/tasks/{id}",
        "/tasks/{id}/cancel",
        "/tasks/{id}/retry",
        "/backends",
        "/runs",
        "/interactions",
        "/interactions/{id}/reply",
        "/events",
        "/openapi.json",
    ] {
        assert!(spec["paths"][path].is_object(), "missing path {path}");
    }
    for schema in [
        "Task",
        "TaskStatus",
        "CreateTaskRequest",
        "Backend",
        "Record",
        "Interaction",
        "Error",
    ] {
        assert!(
            spec["components"]["schemas"][schema].is_object(),
            "missing schema {schema}"
        );
    }
    // TaskStatus 枚举与领域类型一致，防 schema 与实现漂移。
    let statuses: Vec<&str> = spec["components"]["schemas"]["TaskStatus"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        vec![
            "queued",
            "running",
            "retry_scheduled",
            "succeeded",
            "failed",
            "cancelled"
        ]
    );
}

#[tokio::test]
async fn replying_unknown_interaction_conflicts() {
    let (router, _) = setup();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/interactions/missing/reply")
                .header(header::AUTHORIZATION, "Bearer test-token-0123456789abcdef")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"decision":"accept"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}
