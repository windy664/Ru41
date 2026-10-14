# Ru41

Rust 编写的个人自动化助手，围绕 Obsidian 组织个人想法、一手信息和任务。**Codex 是首个 Agent 底座，核心通过 `AgentBackend` 接口接入运行时。**

> 当前 v0.2 已实现持久化任务、Worker 和 Codex 分析任务执行链。尚未接入真实 Vault、Git 自动归档、消息平台或需要审批的外部动作。`succeeded` 表示分析结果已校验并保存，不表示已完成用户描述的任意外部操作。

## 已实现

- 任务创建、去重、查询、取消、失败重试和每次执行的历史。
- SQLite 原子领取、租约续期、过期接管和旧 Worker 写入拦截；存储经 `TaskStore` trait 抽象，SQLite 为默认实现，另提供 PostgreSQL 适配器（`--store postgres`）。
- 常驻 Worker / 单次执行，超时、退避重试和 SIGINT/SIGTERM 停止。
- Codex CLI 子进程：stdin 输入、JSONL 事件、受限大小的日志、结构化结果校验。
- 独立尝试目录；Unix 上取消/超时清理子进程组，其他平台仅保证直接子进程终止。
- 通用底座注册、能力声明、任务按后端路由，以及后端/模型/会话记录。
- QQ 官方机器人消息网关：群@/单聊消息幂等入队为任务，任务到终态后被动回复原会话。
- Agent 调用前信息脱敏：prompt 中的手机号、身份证、邮箱、银行卡号、姓名替换为随机同类假数据，返回结果回填真值后落库；映射表只存本地尝试目录（`mapping.json` 即审计记录），`worker --no-sanitize` 可关闭。
- 旧版数据库自动迁移；自动行为测试与 GitHub Actions。

目前唯一真实适配器是 **Codex**。两个模拟后端验证了公共接口与路由；其他 Agent 不会因接口存在而自动可用。当前适配器不提供会话续接或外部写入工具。

## 快速开始

需要支持 Rust 2024 edition 的工具链；本地验证使用 Rust 1.98.0、Codex CLI 0.162.1。Codex 需提前安装并完成登录。

```bash
git clone https://github.com/windy664/Ru41.git
cd Ru41
cargo build --locked
cargo run --locked -- doctor
cargo run --locked -- backends
```

`doctor` 仅检查 CLI 版本，不验证登录，也不调用模型。`backends` 列出当前编译注册的底座和支持的契约。

创建分析任务并执行：

```bash
cargo run --locked -- task add --key demo:1 --backend codex --prompt "给个人论文阅读计划列出三个建议"
cargo run --locked -- worker --once
cargo run --locked -- task show 1
cargo run --locked -- task attempts 1
```

Worker 会实际调用 Codex，使用已有认证并消耗对应额度。适配器使用 `--ignore-user-config`、`--sandbox read-only` 和独立尝试目录；不加载用户自定义模型后端或 MCP 配置。可通过 `--model` 显式选择账户可用模型。只读沙箱不是独立机器或完整数据隔离。

常驻运行、取消和手动重试：

```bash
cargo run --locked -- worker --timeout-secs 300
# 另一个终端
cargo run --locked -- task list
cargo run --locked -- task cancel 1
# 仅 failed 任务支持手动重试
cargo run --locked -- task retry 1
```

- `--once` 最多领取一个就绪任务；重试调度后的任务需再次运行 Worker 或使用常驻模式。
- 每轮最多尝试 3 次，失败后等待 10、20 秒再重试；耗尽预算进入 `failed`。手动重试追加 3 次预算，保留历史。
- 运行中取消在下一次状态轮询时处理，通常约 1 秒；超时和停止会丢弃运行 future 并清理本地子进程。
- Worker 被强制杀死后，其他 Worker 在 30 秒租约过期后可接管。强杀宿主无法保证旧进程树立即结束，但其结果不能再提交；此版本只支持可重跑的分析任务。
- 同一次输入重试使用相同 `--key`、内容和底座；改变其中内容或底座要使用新键。
- `worker --once` 返回的是一次调度报告，请检查 JSON 的 `status`；一次业务失败不等于 Worker 命令异常退出。

## QQ 消息网关

接入 QQ 官方机器人开放平台（需在平台后台创建机器人并开启「群聊@消息 / C2C 消息」权限）。网关只负责收消息入队和结果回复，任务执行仍需 Worker 进程：

```bash
# 终端 1：网关（凭证也可用环境变量 RU41_QQ_APP_ID / RU41_QQ_APP_SECRET）
cargo run --locked -- qq run --qq-app-id <APP_ID> --qq-app-secret <SECRET>
# 终端 2：照常运行 Worker 执行任务
cargo run --locked -- worker
# 手动发一条主动群消息测试连通性
cargo run --locked -- qq send --qq-app-id <APP_ID> --qq-app-secret <SECRET> --group-openid <GID> --text "ping"
```

- 群里 @机器人 或单聊发送的文本会以 `qq:msg:{msg_id}` 为键幂等入队（`analysis_v1` 契约，默认 `codex` 底座，可 `qq run --backend` 改）；重复投递不会产生重复任务。
- 任务进入 `succeeded`/`failed` 后，网关把结果摘要或错误作为**被动回复**发回原群/原单聊，并记录 `notified_at` 保证不重复发送。
- QQ 被动回复要求带原消息的 `msg_id` 且约 5 分钟内有效；超过窗口或重启较晚的任务可能回复失败（日志可见，任务结果不受影响）。QQ 对主动推送有配额与频控限制，本网关以被动回复为主。协议细节与限制见 [QQ 渠道说明](docs/channels-qq.md)。

运行数据默认位于 `.ru41/state.db` 和 `.ru41/runs/`，可以分别通过 `--db`、`--runs-dir` 指定。不要将运行数据放进 Vault。日志包含任务内容及模型输出，需要按私人数据管理；当前没有自动清理策略。

## 配置文件

除 CLI 参数外，也可以用 TOML 配置文件。加载顺序：`--config <路径>` > `./ru41.toml` > `~/.config/ru41/config.toml`；都没有则用内置默认值。同一配置的取值优先级为 **CLI 参数 > 环境变量 > 配置文件 > 默认值**。`ru41.toml` 已在 `.gitignore` 中（密钥不要进 git），样例见 `ru41.example.toml`：

```toml
[store]        # kind = "sqlite"|"postgres"、path、url
[worker]       # timeout_secs、runs_dir、sanitize
[agent.codex]  # bin、model
[channel.qq]   # app_id、app_secret（建议改用 RU41_QQ_APP_ID / RU41_QQ_APP_SECRET 环境变量）
```

使用 PostgreSQL 作为存储时，通过 `--store postgres --pg-url`（或环境变量 `RU41_PG_URL`、配置 `store.url`）指定连接串，启动时自动执行 `migrations/postgres/` 下的迁移：

```bash
cargo run --locked -- --store postgres --pg-url postgres://user:pass@localhost:5432/ru41 task list
cargo run --locked -- --store postgres worker --once
```

## 核心架构

```text
HTTP API / CLI / QQ 网关 / 后续 Git、定时器入口
                  ↓
        应用服务（TaskService / InteractionService）
                  ↓
     TaskStore（SQLite 默认，可选 PostgreSQL）+ 运行/交互/投递记录
                  ↓
       Worker：租约、重试、恢复；按任务 backend_id 路由
                  ↓
       AgentBackend 通用接口（ports）
                  ↓
    Codex exec 适配器（批处理）/ Codex app-server 适配器（含审批转发）
                  ↓
   结果校验与事务保存；交互请求持久化等待用户决定
                  ↓
   NotifyDispatcher：独立投递记录、按渠道派发（QQ 等）
```

核心概念分离：**Task 是要完成的工作，AgentRun 是一次底座运行（含状态与 session），Interaction 是底座发出的审批/补充信息请求**。一个任务可多次运行；底座运行成功不等于任务成功。审批由底座触发、Ru41 转发：交互记录持久化到数据库，QQ/HTTP 任一入口可回复，回复校验选项、有效期与运行状态后经适配器回传底座。

脱敏在 Agent 边界完成：prompt 出站前替换为随机同类假值，映射只保留在宿主内存与运行日志摘要中，**不落入 Agent 可访问的运行目录**；结果回填真值后落库。

```text
src/
├── domain/           # 任务、尝试、运行、交互与投递的数据与规则
├── app/              # 公共用例：TaskService、InteractionService
├── ports/            # 可替换能力接口：AgentBackend、TaskStore、NotifyChannel、InteractionHandler
├── runtime/          # 长驻执行机制：Worker、NotifyDispatcher
├── privacy/          # 脱敏与回填
├── adapters/
│   ├── agent/            # 底座实现：codex（exec 批处理）、codex_session（app-server 会话）
│   ├── database/         # 存储实现：SQLite、PostgreSQL
│   └── messaging/        # 渠道协议：QQ gateway/api/notifier
├── ingress/
│   ├── http/             # HTTP API（axum，/api/v1，Bearer 认证，SSE 事件流）
│   ├── cli/              # CLI 命令
│   └── qq/               # QQ 消息接收（转公共用例入队）
├── bootstrap/        # 配置加载
├── util/             # 少量无业务规则的工具
└── main.rs           # 装配：配置 → 存储 → 底座注册表 → 入口
```

职责约束：`app` 只描述“做什么”；`runtime` 负责“如何持续执行”；`ports` 定义可替换边界；`adapters` 翻译外部协议，不决定业务成功或审核通过；所有入口（HTTP/CLI/QQ）调用同一套应用服务。

## HTTP API

```bash
export RU41_API_TOKEN=$(openssl rand -hex 24)   # 至少 24 字节
cargo run --locked -- serve --listen 127.0.0.1:4141

curl -H "Authorization: Bearer $RU41_API_TOKEN" http://127.0.0.1:4141/api/v1/tasks
curl -X POST -H "Authorization: Bearer $RU41_API_TOKEN" -H "Content-Type: application/json" \
  -d '{"key":"api:1","prompt":"整理收件箱","backend":"codex"}' \
  http://127.0.0.1:4141/api/v1/tasks
curl -X POST -H "Authorization: Bearer $RU41_API_TOKEN" -H "Content-Type: application/json" \
  -d '{"decision":"accept"}' \
  http://127.0.0.1:4141/api/v1/interactions/{id}/reply
```

端点：`GET/POST /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`POST /api/v1/tasks/{id}/cancel|retry`、`GET /api/v1/backends`、`GET /api/v1/runs`、`GET /api/v1/interactions`、`POST /api/v1/interactions/{id}/reply`、`GET /api/v1/events`（SSE）、`GET /api/v1/openapi.json`。API 与 CLI、QQ 共用同一套应用服务与存储；任务由独立 worker 进程执行，serve 只做入口。

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

自动测试使用模拟 Agent 或本地假进程，不调用付费模型、不访问真实 Vault。覆盖数据库升级、并发领取、租约接管、取消竞态、重试预算、协议错误、日志上限、进程清理与多底座路由。
