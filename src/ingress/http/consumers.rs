//! 消费者运维接口：查看进度与恢复重试耗尽的事件，不允许任意跳过游标。
use super::{ApiError, ApiState, blocking};
use crate::domain::consumers::ConsumerState;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

pub(super) async fn get_consumer(
    State(s): State<ApiState>,
    Path(name): Path<String>,
) -> Result<Json<ConsumerState>, ApiError> {
    let value = blocking(move || s.store.consumer(&name))
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "consumer_not_found"))?;
    Ok(Json(value))
}
pub(super) async fn retry_consumer(
    State(s): State<ApiState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let changed =
        blocking(move || s.store.retry_consumer(&name, crate::runtime::worker::now())).await?;
    if !changed {
        return Err(ApiError(StatusCode::CONFLICT, "consumer_not_blocked"));
    }
    Ok(StatusCode::NO_CONTENT)
}
