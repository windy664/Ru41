# 可靠事件消费者

模块实现 `ports::consumers::EventHandler`，由 `runtime::consumers::ConsumerWorker` 驱动；SQLite 和 PostgreSQL 实现相同的 `ConsumerStore` 契约。处理器获得事件及稳定幂等键，成功返回后才持久确认游标。现有 QQ/飞书 NotifyDispatcher 尚未切换，避免同时启用两条通知链路。

## 语义

- 每个 ConsumerSpec.name 是独立的持久订阅：不同名字各自收到事件，同名进程竞争同一个消费进度。
- start_after=0 从现有日志开头消费；只收新事件时先读取 EventStore.event_cursor() 并把结果作为注册起点。注册不允许静默修改已有名字的起点和 max_attempts。
- 同一个消费者顺序处理，一次只有一条有效租约。处理器不关心的事件返回成功即可推进进度；不能启动后台副作用后立即返回成功。
- 每次认领递增 generation 和尝试次数。确认、失败、续租都校验消费者名、事件 ID、代次和未过期租约，旧进程无法覆盖新进度。
- 失败保留当前事件，按指数退避重试（上限一小时）。超时、关闭会丢弃处理 future 并记录固定失败类别，不保存异常正文。
- 崩溃不确认；租约过期后重投相同事件，仍计入尝试预算。预算耗尽停在原事件，blocked=true；最后一次崩溃的耗尽状态在下次认领时落库。
- 显式恢复只重置当前事件的尝试预算，不跳过事件、清空历史日志或移动已确认游标。
- Worker 在处理期间续租，失去租约即停止本地处理。存储故障向宿主返回错误，由宿主决定重启策略，已持久进度不会丢失。

这是至少一次执行，不是外部副作用的恰好一次保证。下游成功但宿主尚未确认时发生崩溃，会再次投递；`delivery.idempotency_key()` 对同一消费者/事件的重试保持不变。处理器应把它传给支持幂等的下游，或与自己的业务写入同事务记录。远端不支持幂等时仍可能重复发送，单独记一个“已发送”标记不能消除该窗口。键只在同一数据库历史内稳定，替换数据库时需重新规划消费者身份与下游去重命名空间。

## 接入

```text
业务模块 EventHandler
       ↑
ConsumerWorker（续租、超时、退避、关闭）
       ↓
ConsumerStore（消费状态）+ EventStore（事件日志）
       ↓
SQLite / PostgreSQL
```

独立模块持有自己的 ConsumerWorker 并调用 run_loop(shutdown)。bootstrap 显式装配处理器；这个版本不提供动态插件发现，也不在主进程自动启动未配置的处理器。操作型接口不开放任意注册、跳过游标或伪造确认。

可运行示例，每次执行处理下一条事件；使用测试数据库验证，避免修改正在使用的消费者进度：

```sh
cargo run --example event_consumer_once -- /tmp/ru41-test.db example.audit
```

示例代码：[examples/event_consumer_once.rs](../examples/event_consumer_once.rs)。示例只打印事件元数据，不调用 Agent 或发送渠道消息。

## 运维 API

沿用现有 Bearer 鉴权：

- `GET /api/v1/consumers/{name}`：返回 cursor、active_event_id、attempts、generation、lease_until、available_at、blocked、last_failure；不存在返回 404。
- `POST /api/v1/consumers/{name}/retry`：恢复重试耗尽的订阅，成功返回 204；不存在或未阻塞返回 409。

接口已加入 OpenAPI。last_failure 仅包含 handler_failed、timeout、interrupted、lease_expired；详细错误由模块自己的受控诊断机制处理。

第一版不会并行处理同一订阅、跳过失败事件、自动清理事件日志或保证外部消息绝不重复。渠道迁移时应先停用旧发送入口，再接新处理器，并明确旧未通知任务的处理方式。
