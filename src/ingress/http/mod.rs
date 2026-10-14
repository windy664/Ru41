mod consumers;
mod events;
use events::events;
mod openapi;
use crate::{
    app::interactions::InteractionService,
    app::task::TaskService,
    ports::{agent::BackendRegistry, storage::TaskStore},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use openapi::openapi_spec;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
/// HTTP 入口的共享状态：存储、底座注册表与 Bearer 令牌。
pub struct ApiState {
    pub store: Arc<dyn TaskStore>,
    pub backends: Arc<BackendRegistry>,
    token: Arc<str>,
}
impl ApiState {
    /// 构造 API 状态；令牌至少 24 字节，防弱令牌误配。
    pub fn new(
        store: Arc<dyn TaskStore>,
        backends: Arc<BackendRegistry>,
        token: String,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            token.len() >= 24,
            "API token must contain at least 24 bytes"
        );
        Ok(Self {
            store,
            backends,
            token: token.into(),
        })
    }
}
/// 构建 `/api/v1` 路由：任务 CRUD、底座、运行、交互回复、SSE、OpenAPI。
/// 全部经 Bearer 认证；与 CLI/QQ 共用同一应用服务。
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route("/api/v1/tasks/{id}", get(get_task))
        .route("/api/v1/tasks/{id}/cancel", post(cancel_task))
        .route("/api/v1/tasks/{id}/retry", post(retry_task))
        .route("/api/v1/backends", get(backends))
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/interactions", get(interactions))
        .route("/api/v1/interactions/{id}/reply", post(reply))
        .route("/api/v1/events", get(events))
        .route("/api/v1/consumers/{name}", get(consumers::get_consumer))
        .route(
            "/api/v1/consumers/{name}/retry",
            post(consumers::retry_consumer),
        )
        .route("/api/v1/openapi.json", get(openapi))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}
async fn authenticate(State(state): State<ApiState>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mismatch = state
        .token
        .as_bytes()
        .iter()
        .zip(supplied.as_bytes())
        .fold(0_u8, |v, (a, b)| v | (a ^ b));
    if supplied.len() != state.token.len() || mismatch != 0 {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"unauthorized"})),
        )
            .into_response();
    }
    next.run(request).await
}
struct ApiError(StatusCode, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?
        .map_err(|e| {
            eprintln!("[api] {e:#}");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        })
}
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateTask {
    key: String,
    prompt: String,
    backend: String,
}
#[derive(Serialize, schemars::JsonSchema)]
struct TaskDto {
    id: i64,
    backend: String,
    status: crate::domain::TaskStatus,
    summary: Option<String>,
}
impl From<crate::domain::Task> for TaskDto {
    fn from(t: crate::domain::Task) -> Self {
        Self {
            id: t.id,
            backend: t.backend_id,
            status: t.status,
            summary: t.result.map(|r| r.summary),
        }
    }
}
async fn create_task(
    State(s): State<ApiState>,
    Json(input): Json<CreateTask>,
) -> Result<(StatusCode, Json<TaskDto>), ApiError> {
    if input.key.trim().is_empty()
        || input.key.len() > 512
        || input.prompt.trim().is_empty()
        || input.prompt.len() > 65536
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_input"));
    }
    if s.backends.get(&input.backend).is_err() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "unknown_backend"));
    }
    let result = blocking(move || {
        TaskService::new(s.store).enqueue_for(&input.key, &input.prompt, &input.backend)
    })
    .await?;
    Ok((
        if result.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(result.task.into()),
    ))
}
#[derive(Deserialize)]
struct Page {
    #[serde(default = "default_limit")]
    limit: u32,
}
fn default_limit() -> u32 {
    20
}
async fn list_tasks(
    State(s): State<ApiState>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<TaskDto>>, ApiError> {
    if !(1..=1000).contains(&page.limit) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_limit"));
    }
    Ok(Json(
        blocking(move || TaskService::new(s.store).list(page.limit))
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}
async fn get_task(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<TaskDto>, ApiError> {
    let task = blocking(move || s.store.get(id))
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "task_not_found"))?;
    Ok(Json(task.into()))
}
async fn cancel_task(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<TaskDto>, ApiError> {
    Ok(Json(
        blocking(move || TaskService::new(s.store).cancel(id))
            .await?
            .into(),
    ))
}
async fn retry_task(
    State(s): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<TaskDto>, ApiError> {
    Ok(Json(
        blocking(move || TaskService::new(s.store).retry(id))
            .await?
            .into(),
    ))
}
async fn backends(State(s): State<ApiState>) -> Json<Value> {
    Json(json!(s.backends.descriptors()))
}
async fn runs(State(s): State<ApiState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(
        blocking(move || s.store.documents("run", 100)).await?
    )))
}
async fn interactions(State(s): State<ApiState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(
        blocking(move || s.store.documents("interaction", 100)).await?
    )))
}
async fn reply(
    State(s): State<ApiState>,
    Path(id): Path<String>,
    Json(response): Json<Value>,
) -> Result<StatusCode, ApiError> {
    let result = tokio::task::spawn_blocking(move || {
        InteractionService { store: s.store }.reply(&id, response)
    })
    .await
    .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    result.map_err(|_| ApiError(StatusCode::CONFLICT, "invalid_or_stale_interaction"))?;
    Ok(StatusCode::NO_CONTENT)
}
/// OpenAPI 契约：schema 由 schemars 从 DTO 类型生成（单一事实来源），
/// paths 在此集中维护。
async fn openapi() -> Json<Value> {
    Json(openapi_spec())
}
