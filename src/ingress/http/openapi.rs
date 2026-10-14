use super::{CreateTask, TaskDto};
use serde_json::{Value, json};

pub(super) fn openapi_spec() -> Value {
    let err = json!({"type": "object", "required": ["error"], "properties": {"error": {"type": "string"}}});
    let task_array = json!({"type": "array", "items": {"$ref": "#/components/schemas/Task"}});
    let mut spec = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Ru41 API",
            "version": "0.3.0",
            "description": "Ru41 个人自动化底座的 HTTP API。与 CLI、QQ 网关共用同一套应用服务与存储；任务由独立 worker 进程执行。"
        },
        "servers": [{"url": "/api/v1"}],
        "security": [{"bearerAuth": []}],
        "components": {
            "securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer",
                "description": "环境变量 RU41_API_TOKEN 或配置 [api].token（至少 24 字节）"}},
            "schemas": {
                "Error": err,
                "Task": component_schema::<TaskDto>(),
                "TaskStatus": component_schema::<crate::domain::TaskStatus>(),
                "CreateTaskRequest": component_schema::<CreateTask>(),
                "Backend": component_schema::<crate::ports::agent::BackendDescriptor>(),
                "Record": component_schema::<crate::domain::records::Document>(),
                "Interaction": component_schema::<crate::domain::records::Interaction>(),
                "DomainEvent": component_schema::<crate::domain::events::DomainEvent>()
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
                    "responses": {"200": {"description": "交互列表", "content": {"application/json": {"schema": {"type": "array", "items": {"$ref": "#/components/schemas/Record"}}}}}, "401": {"description": "未认证"}}}
            },
            "/interactions/{id}/reply": {
                "post": {"summary": "回复交互请求（审批决定等）",
                    "description": "回复必须是该交互 options 之一；请求过期、已回答、运行不活跃时返回 409。",
                    "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                    "requestBody": {"required": true, "content": {"application/json": {"schema": {"description": "交互允许的选项之一，如 {\"decision\":\"accept\"}"}}}},
                    "responses": {"204": {"description": "已记录"}, "409": {"description": "无效或过期的交互"}, "401": {"description": "未认证"}}}
            },
            "/events": {
                "get": {"summary": "SSE 持久化业务事件流",
                    "description": "按提交顺序推送增量事件；SSE id 是数据库内游标，event 是 kind，data 为 DomainEvent JSON。Last-Event-ID 优先于 after；缺省从 0 回放。无历史快照或原始 Agent 正文。迁移前事件不补录。",
                    "parameters": [
                        {"name":"after","in":"query","schema":{"type":"integer","format":"int64","minimum":0,"default":0}},
                        {"name":"Last-Event-ID","in":"header","schema":{"type":"string","pattern":"^[0-9]+$"}}
                    ],
                    "responses": {"400":{"description":"无效、负数或超前于当前数据库的游标"},"200": {"description": "text/event-stream；每个 data 对应 DomainEvent，错误事件 events_unavailable 不推进游标", "content": {"text/event-stream": {"schema": {"type": "string"}}}}}}
            },
            "/openapi.json": {
                "get": {"summary": "本规范", "responses": {"200": {"description": "OpenAPI 文档"}}}
            }
        }
    });
    let schemas = spec["components"]["schemas"]
        .as_object_mut()
        .expect("schemas object");
    let mut definitions = serde_json::Map::new();
    for schema in schemas.values_mut() {
        if let Some(local) = schema.as_object_mut().and_then(|s| s.remove("definitions")) {
            definitions.extend(local.as_object().expect("definitions object").clone());
        }
    }
    for (name, schema) in definitions {
        schemas.entry(name).or_insert(schema);
    }
    for path in spec["paths"]
        .as_object_mut()
        .expect("paths object")
        .values_mut()
    {
        for operation in path.as_object_mut().expect("path item").values_mut() {
            operation["responses"]["401"] = json!({"description":"未认证","content":{"application/json":{"schema":{"$ref":"#/components/schemas/Error"}}}});
        }
    }
    spec
}

/// 引用指向 OpenAPI components，避免嵌入 RootSchema 后留下悬空 definitions 引用。
fn component_schema<T: schemars::JsonSchema>() -> Value {
    let mut settings = schemars::r#gen::SchemaSettings::draft2019_09();
    settings.definitions_path = "#/components/schemas/".into();
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("schema serialization");
    value
        .as_object_mut()
        .expect("schema object")
        .remove("$schema");
    value
}
