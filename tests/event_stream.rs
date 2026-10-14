use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use futures_util::StreamExt;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    ingress::http::{ApiState, router},
    ports::{agent::BackendRegistry, events::EventStore, storage::TaskStore},
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

fn setup() -> (tempfile::TempDir, axum::Router, Arc<SqliteTaskStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("events.db")).unwrap());
    let app = router(
        ApiState::new(
            store.clone(),
            Arc::new(BackendRegistry::default()),
            "event-token-0123456789abcdef".into(),
        )
        .unwrap(),
    );
    (dir, app, store)
}
fn request(uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .uri(uri)
        .header("authorization", "Bearer event-token-0123456789abcdef")
}

#[tokio::test]
async fn replay_then_live_events_from_another_connection_and_resume() {
    let (dir, app, store) = setup();
    store.enqueue("one", "secret-prompt").unwrap();
    let first = store.event_cursor().unwrap();
    let response = app
        .clone()
        .oneshot(request("/api/v1/events").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut stream = response.into_body().into_data_stream();
    let data = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(data.to_vec()).unwrap();
    assert!(text.contains(&format!("id: {first}")));
    assert!(text.contains("event: task.queued"));
    assert!(!text.contains("secret-prompt"));
    let writer = SqliteTaskStore::open(&dir.path().join("events.db")).unwrap();
    writer.enqueue("two", "other-secret").unwrap();
    let second = writer.event_cursor().unwrap();
    let data = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(data.to_vec())
            .unwrap()
            .contains(&format!("id: {second}"))
    );
    drop(stream);
    // 浏览器重连头优先于最初 URL 中的 after 参数。
    let response = app
        .oneshot(
            request("/api/v1/events?after=0")
                .header("last-event-id", first.to_string())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    let data = tokio::time::timeout(Duration::from_secs(3), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        String::from_utf8(data.to_vec())
            .unwrap()
            .contains(&format!("id: {second}"))
    );
}

#[tokio::test]
async fn cursor_validation_and_authentication() {
    let (_dir, app, _store) = setup();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    for uri in [
        "/api/v1/events?after=-1",
        "/api/v1/events?after=1",
        "/api/v1/events?after=invalid",
    ] {
        let response = app
            .clone()
            .oneshot(request(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    for cursor in ["bad", "-1", "99999999999999999999999999999999"] {
        let response = app
            .clone()
            .oneshot(
                request("/api/v1/events")
                    .header("last-event-id", cursor)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn replay_pages_without_skipping_events() {
    let (_dir, app, store) = setup();
    for n in 0..105 {
        store.enqueue(&format!("page:{n}"), "x").unwrap();
    }
    let expected = store.events_after(0, 1000).unwrap();
    let response = app
        .oneshot(
            request("/api/v1/events?after=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut stream = response.into_body().into_data_stream();
    for event in expected {
        let data = tokio::time::timeout(Duration::from_secs(3), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = String::from_utf8(data.to_vec()).unwrap();
        assert!(text.contains(&format!("id: {}\n", event.id)), "{text}");
    }
}
