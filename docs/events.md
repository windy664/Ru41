# 持久化业务事件与 SSE

任务和审核状态变化写入数据库事件日志。`GET /api/v1/events` 推送增量事件，不再推送每秒一次的 `snapshot`；已有快照客户端需要改为按事件引用查询任务或审核记录。REST 查询接口仍返回当前状态。

## 契约

```text
id: 42
event: task.succeeded
data: {"id":42,"version":1,"kind":"task.succeeded","occurred_at":"2026-10-14T12:00:00.000Z","resource_id":"7","task_id":7,"attempt_id":9,"run_id":"run-7-9","status":"succeeded"}
```

- 任务事件：`task.queued`、`task.running`、`task.retry_scheduled`、`task.succeeded`、`task.failed`、`task.cancelled`。手动重试回到 queued；租约恢复也记录实际状态变化。
- 审核事件：`interaction.requested`、`interaction.answered`、`interaction.expired`。只记录底座交互的生命周期，不决定审核策略。
- `version=1` 是事件内容版本；`id` 是当前数据库内的提交顺序游标。重建或替换数据库时应重置客户端游标。
- `resource_id` 是任务或审核资源 ID。`task_id`、`attempt_id`、`run_id` 在可关联时提供；初次入队没有运行批次。
- 事件不包含 prompt、结果正文、错误正文、审核 detail/options/response、密钥或脱敏映射。需要正文时通过已有鉴权 REST 查询接口获取。
- 重复入队、失败的乐观并发更新、无状态变化的重复取消、心跳、被 fencing 拒绝的旧 Worker 写入不产生事件。

## 订阅与恢复

```sh
curl -N -H "Authorization: Bearer $RU41_API_TOKEN" \
  'http://127.0.0.1:4141/api/v1/events?after=0'

curl -N -H "Authorization: Bearer $RU41_API_TOKEN" \
  -H 'Last-Event-ID: 42' \
  'http://127.0.0.1:4141/api/v1/events'
```

`Last-Event-ID` 优先于 `after`；缺省为 0，从第一条已记录事件回放，然后持续读取新事件。负数、非法整数及大于当前数据库最大 ID 的游标返回 400。客户端应在处理完成后保存最后的 ID，并按 ID 去重；连接成功不代表业务处理已完成。当前使用原有全局 Bearer 认证，未新增多用户权限体系。

每次最多缓存 100 条，消费者慢时不会在内存积累全部历史。空日志每 500ms 再读，连接有 SSE keep-alive；读取故障发出 `error / events_unavailable`，不推进游标并继续重试。服务重启、API/Worker 分进程时，客户端仍可读取同一数据库日志。浏览器原生 EventSource 无法直接设置 Bearer header，前端应使用支持自定义请求头的流式客户端，勿把令牌放进 URL。

## 持久化边界

`domain/events.rs` 定义事件，`ports/events.rs::EventStore` 定义读取契约。`TaskStore` 继承该端口，新增数据库适配器必须同时满足事件与状态更新的原子性。SQLite/PostgreSQL 使用迁移内的触发器覆盖任务与审核的状态变化；无“状态先提交、随后单独 publish”的双写窗口。事件写入失败时，业务更新也回滚。

SQLite 的写事务串行化。PostgreSQL 使用事务计数器行，序号分配持锁至事务结束，避免普通 sequence 的分配顺序和提交顺序不一致而漏读。这个设计会串行化事件写入，适合当前个人自动化规模；以后扩展吞吐量时必须保留提交顺序或重新设计游标协议。

迁移前任务保留，不虚构历史事件；首次客户端应同时使用 REST 查询当前资源。第一版事件不清理，日志会持续增长；保留策略与“游标已过期”的恢复协议需要一起设计。事件日志用于通知和追踪，不用于重建全部业务状态。

## 验证与后续

共享测试验证 SQLite/PostgreSQL 状态语义、重复操作、租约接管、敏感正文隔离。另有事务回滚、事件写入失败、数据库升级、跨连接读取、分页、SSE 续传与鉴权测试；PostgreSQL 增加并发提交顺序测试。

消费者确认、租约与重试已实现，见 [可靠事件消费者](event-consumers.md)。Agent token 流、工具执行进度以及 QQ/飞书迁移到事件分发仍待实现。消费者复用事件 ID 和版本契约，不直接订阅底座原始协议。
