//! 持久化事件的 SSE 出口。逐批读取共享数据库，支持独立 Worker 与断线续传。
use super::{ApiError, ApiState, blocking};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
};
use serde::Deserialize;
use std::{collections::VecDeque, convert::Infallible, time::Duration};

#[derive(Default, Deserialize)]
pub(super) struct Cursor {
    #[serde(default)]
    after: i64,
}

/// Last-Event-ID 优先于 after；缺省从迁移后的第一条事件开始。
pub(super) async fn events(
    State(state): State<ApiState>,
    Query(query): Query<Cursor>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let after = match headers.get("last-event-id") {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_event_cursor"))?,
        None => query.after,
    };
    let store = state.store.clone();
    let latest = blocking(move || store.event_cursor()).await?;
    if after < 0 || after > latest {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_event_cursor"));
    }
    let stream = futures_util::stream::unfold(
        (
            state.store,
            after,
            VecDeque::<crate::domain::events::DomainEvent>::new(),
        ),
        |(store, mut cursor, mut pending)| async move {
            loop {
                if let Some(event) = pending.pop_front() {
                    cursor = event.id;
                    let frame = Event::default()
                        .id(event.id.to_string())
                        .event(&event.kind)
                        .data(
                            serde_json::to_string(&event)
                                .expect("event contains only serializable fields"),
                        );
                    return Some((Ok(frame), (store, cursor, pending)));
                }
                let reader = store.clone();
                match tokio::task::spawn_blocking(move || reader.events_after(cursor, 100)).await {
                    Ok(Ok(batch)) if !batch.is_empty() => pending.extend(batch),
                    Ok(Ok(_)) => tokio::time::sleep(Duration::from_millis(500)).await,
                    _ => {
                        // 错误不推进游标；重试仍从最后一条成功输出的事件继续。
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        return Some((
                            Ok(Event::default().event("error").data("events_unavailable")),
                            (store, cursor, pending),
                        ));
                    }
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
