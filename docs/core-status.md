# 核心宿主状态与并行开发边界

Ru41 托管 Agent，不实现模型推理循环、底座记忆或底座审批策略。渠道通过应用服务提交任务和回复交互；底座适配器翻译原生协议。

## 当前形态（2026-11-03 插件化完成后）

- **src/modules/ 已清空**：全部附属以插件（.so）挂载，内建注册表只剩 linkme 空壳。
  迁移清单与操作手册见 `docs/plugin-development.md` 附录 checklist。
- 附属的 poll/run 走 `ru41 <模块名> poll|run`，专属子命令走各模块 `DEF.cli` 钩子
  （外部子命令统一分发，core 无硬编码命令）。**插件一切异步入口必须过
  `ru41::plugins::bridge`**（插件静态链接自己的 tokio，reactor 与宿主不通用）。
- 跨附属数据契约住 core：`domain/timetable.rs`（课表快照）、`adapters/cas.rs`
  （验证码+RSA）、`adapters/sources/`（多消费者共享的信息源协议层，rss 查表分发）。
- 动态插件握手：api crate 版本 + deps stamp 硬门（bump 规则见 plugin-development.md）。
- 容器可按角色过滤加载：`RU41_PLUGINS=jwxt,planner` 环境变量白名单。

## 尚未完成，不能视为成熟能力

1. 交互回复和任务租约检查不是同一数据库事务；旧 attempt 与新 attempt 的关联还需原子验证。等待者消费回复前也应重新验证运行有效性。
2. Worker 崩溃后的 AgentRun、Interaction 状态尚无完整恢复协调；不能把数据库中的 running 当作底座仍存活的证据。
3. records 是通用 JSON 存储，缺少独立类型化运行/交互/投递仓储及必要查询索引。
4. PostgreSQL 同步客户端的 runtime 桥接与析构管理需要替换，避免资源泄漏和阻塞执行线程。
5. HTTP 业务错误仍未全面映射成 400/404/409。SSE 已支持任务/审核持久化事件及游标续传；可靠消费者运行器与运维接口已实现；Agent 过程事件及渠道消费者接入待实现，见 events.md 与 event-consumers.md。
6. Codex 会话协议需要独立模拟进程测试与真实冒烟；底座不支持的交互不可冒充支持。
7. 任务处理仍以 analysis_v1 为主，尚未形成业务处理器注册与可恢复步骤执行。
8. 投递发送成功但登记前崩溃仍可能重发；平台幂等与回执核验需要按渠道能力实现。

## 附属侧的既有技术债（插件化后新产生）

- digest 去重状态机（`app::digest`）仅 2/23 插件使用，其余手写 put_document
  seen-set（blog 的 done_key 已演化到 v3）；推广 campaign 待做，接口已就绪。
- 知识库自动索引原来由 obsidian 服务挂载，插件化后插件内无 ONNX——已改为
  服务器 cron 每天 03:23 在 obsidian 容器内跑宿主 CLI `knowledge index`。
- `RU41_PLUGINS` 角色过滤已实现，生产容器逐个配置白名单的 chore 待做
  （默认全量加载，行为不变）。

## 并行开发约定

本轮不改变 TaskStore、InteractionHandler、NotifyChannel 的公开签名，不修改 QQ/飞书、身份绑定或启动配置。后续涉及契约变化时提供迁移说明与测试，避免渠道开发追随未经验证的接口。
