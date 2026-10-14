# 核心宿主状态与并行开发边界

Ru41 托管 Agent，不实现模型推理循环、底座记忆或底座审批策略。渠道通过应用服务提交任务和回复交互；底座适配器翻译原生协议。

## 本轮修复

- OpenAPI schema 的嵌套引用统一指向 components/schemas，并收集所有定义；测试递归验证引用可解析。
- OpenAPI 声明 /api/v1 服务前缀。规范入口与其余入口一样需要认证。
- /interactions 的响应保持现有 Document[] 包装（id、revision、value），文档与实际返回一致，不改变渠道接口。
- HTTP 契约生成移至 ingress/http/openapi.rs。
- Worker 保留底座返回的 session_id 至 AgentRun。非法结果由宿主分类为永久失败；超时仍走有界重试。

## 尚未完成，不能视为成熟能力

1. 交互回复和任务租约检查不是同一数据库事务；旧 attempt 与新 attempt 的关联还需原子验证。等待者消费回复前也应重新验证运行有效性。
2. Worker 崩溃后的 AgentRun、Interaction 状态尚无完整恢复协调；不能把数据库中的 running 当作底座仍存活的证据。
3. records 是通用 JSON 存储，缺少独立类型化运行/交互/投递仓储及必要查询索引。
4. PostgreSQL 同步客户端的 runtime 桥接与析构管理需要替换，避免资源泄漏和阻塞执行线程。
5. HTTP 业务错误仍未全面映射成 400/404/409。SSE 已支持任务/审核持久化事件及游标续传；Agent 过程事件与可靠消费者分发待实现，见 events.md。
6. Codex 会话协议需要独立模拟进程测试与真实冒烟；底座不支持的交互不可冒充支持。
7. 任务处理仍以 analysis_v1 为主，尚未形成业务处理器注册与可恢复步骤执行。
8. 投递发送成功但登记前崩溃仍可能重发；平台幂等与回执核验需要按渠道能力实现。

## 并行开发约定

本轮不改变 TaskStore、InteractionHandler、NotifyChannel 的公开签名，不修改 QQ/飞书、身份绑定或启动配置。后续涉及契约变化时提供迁移说明与测试，避免渠道开发追随未经验证的接口。
