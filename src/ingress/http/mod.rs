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
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};

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
async fn events(
    State(s): State<ApiState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let stream = futures_util::stream::unfold(s, |s| async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let store = s.store.clone();
        let result=tokio::task::spawn_blocking(move||->anyhow::Result<Value>{Ok(json!({"tasks":store.list(100)?.into_iter().map(TaskDto::from).collect::<Vec<_>>(),"interactions":store.documents("interaction",100)?}))}).await;
        let event = match result {
            Ok(Ok(value)) => Event::default().event("snapshot").data(value.to_string()),
            _ => Event::default().event("error").data("snapshot_unavailable"),
        };
        Some((Ok(event), s))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
/// OpenAPI 契约：schema 由 schemars 从 DTO 类型生成（单一事实来源），
/// paths 在此集中维护。
async fn openapi() -> Json<Value> {
    Json(openapi_spec())
}

fn openapi_spec() -> Value {
    use schemars::schema_for;
    let err = json!({"type": "object", "required": ["error"], "properties": {"error": {"type": "string"}}});
    let task_array = json!({"type": "array", "items": {"$ref": "#/components/schemas/Task"}});
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Ru41 API",
            "version": "0.3.0",
            "description": "Ru41 个人自动化底座的 HTTP API。与 CLI、QQ 网关共用同一套应用服务与存储；任务由独立 worker 进程执行。"
        },
        "security": [{"bearerAuth": []}],
        "components": {
            "securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer",
                "description": "环境变量 RU41_API_TOKEN 或配置 [api].token（至少 24 字节）"}},
            "schemas": {
                "Error": err,
                "Task": schema_for!(TaskDto),
                "TaskStatus": schema_for!(crate::domain::TaskStatus),
                "CreateTaskRequest": schema_for!(CreateTask),
                "Backend": schema_for!(crate::ports::agent::BackendDescriptor),
                "Record": schema_for!(crate::domain::records::Document),
                "Interaction": schema_for!(crate::domain::records::Interaction)
            }
        },
        "paths": {
            "/tasks": {
                "get": {"summary": "列出最近任务", "parameters": [
                    {"name": "limit", "in": "query", "schema": {"type": "integer", "default": 20, "minimum": 1, "maximum": 1000}}],
                    "responses": {"200": {"description": "任务列表（id 降序）", "content": {"application/json": {"schema": task_array}}}}},
                "post": {"summary": "提交任务（幂等）",
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"$ref": "#/components/schemas/CreateTaskRequest"}}}},
                    "responses": {"201": {"description": "已创建", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Task"}}}},
                                  "200": {"description": "幂等命中既有任务", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Task"}}}},
                                  "400": {"description": "输入或底座非法"}, "401": {"description": "未认证"}}}
            },
            "/tasks/{id}": {
                "get": {"summary": "查询单个任务",
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer", "format": "int64"}}],
                    "responses": {"200": {"description": "任务", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Task"}}}},
                                  "404": {"description": "不存在"}, "401": {"description": "未认证"}}}
            },
            "/tasks/{id}/cancel": {
                "post": {"summary": "取消任务（重复取消安全）",
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer", "format": "int64"}}],
                    "responses": {"200": {"description": "取消后的任务", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Task"}}}}, "401": {"description": "未认证"}}}
            },
            "/tasks/{id}/retry": {
                "post": {"summary": "手动重试失败任务（追加三次预算）",
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "integer", "format": "int64"}}],
                    "responses": {"200": {"description": "重试后的任务", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Task"}}}}, "401": {"description": "未认证"}}}
            },
            "/backends": {
                "get": {"summary": "已注册的 Agent 底座及能力",
                    "responses": {"200": {"description": "底座列表", "content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/Backend"}}}}}, "401": {"description": "未认证"}}}
            },
            "/runs": {
                "get": {"summary": "最近的 AgentRun 记录",
                    "responses": {"200": {"description": "运行记录", "content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/Record"}}}}}, "401": {"description": "未认证"}}}
            },
            "/interactions": {
                "get": {"summary": "交互请求记录（含 pending 待回复）",
                    "responses": {"200": {"description": "交互列表", "content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/Interaction"}}}}}, "401": {"description": "未认证"}}}
            },
            "/interactions/{id}/reply": {
                "post": {"summary": "回复交互请求（审批决定等）",
                    "description": "回复必须是该交互 options 之一；请求过期、已回答、运行不活跃时返回 409。",
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"description": "交互允许的选项之一，如 {\"decision\":\"accept\"}"}}}},
                    "responses": {"204": {"description": "已记录"}, "409": {"description": "无效或过期的交互"}, "401": {"description": "未认证"}}}
            },
            "/events": {
                "get": {"summary": "SSE 事件流（任务与交互快照）",
                    "description": "每秒推送 snapshot 事件，内容为 {tasks:[Task], interactions:[Record]}。",
                    "responses": {"200": {"description": "text/event-stream", "content": {"text/event-stream": {"schema": {"type": "string"}}}}}}
            },
            "/openapi.json": {
                "get": {"summary": "本规范", "security": [], "responses": {"200": {"description": "OpenAPI 文档"}}}
            }
        }
    })
}
