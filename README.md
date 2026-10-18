<div align="center">

# Ru41

**把日常信息交给流程，把注意力留给自己。**

Rust 个人自动化宿主 · 可替换 Agent · 模块化信息处理

[![Rust checks](https://github.com/windy664/Ru41/actions/workflows/ci.yml/badge.svg)](https://github.com/windy664/Ru41/actions/workflows/ci.yml)
![Rust](https://img.shields.io/badge/Rust-2024_edition-d99a6c?style=flat-square)
![Agents](https://img.shields.io/badge/Agents-Codex_%2F_Pi-62bda8?style=flat-square)
![Storage](https://img.shields.io/badge/Storage-SQLite_%2F_PostgreSQL-779fc5?style=flat-square)

[快速开始](#快速开始) · [能力概览](#能力概览) · [核心架构](#核心架构) · [部署指南](deploy/README.md) · [文档索引](docs/README.md)

![Ru41 任务流程：输入经 Rust 宿主排队与脱敏，调用 Codex 或 Pi，校验并保存结果后通过 QQ 或飞书通知；底层支持 SQLite、PostgreSQL 与持久化事件。](docs/assets/task-flow.gif)

<sub>流程示意，非实时监控 · <a href="docs/assets/task-flow.png">查看静态图片</a></sub>

</div>

Ru41 连接消息、信息来源与 Agent 应用：由 Rust 宿主管理任务、存储、脱敏、事件和通知，让 Codex 或 Pi 完成分析。业务模块按需接入，入口与执行相互独立。

> **当前阶段 · v0.2**：任务执行、消息通知与多个业务模块已落地。Obsidian Vault、Git 自动归档与需要审批的外部写入动作仍待接入。`succeeded` 表示分析结果已校验并保存。详见[核心宿主状态](docs/core-status.md)。

## 能力概览

| 方向 | 当前能力 | 了解更多 |
| :--- | :--- | :--- |
| 消息与通知 | QQ 群 @ / 单聊入队，任务结果回复；飞书群 webhook 推送 | [QQ](docs/channels-qq.md) · [飞书](docs/channels-feishu.md) |
| 一手信息 | arXiv 论文追踪与单篇精读，RSS / Atom 筛选与摘要 | [论文](docs/sources-arxiv.md) · [RSS](docs/sources-rss.md) |
| 宿舍日常 | 校园卡平台水电余额、历史读数、低余额提醒 | [水电监控](docs/ecard-waterfee.md) |
| Agent 接入 | Codex 与可选 Pi，共用任务管线；Codex 支持自定义模型 API | [底座接入](docs/agent-backends.md) · [Pi](docs/agent-pi.md) |
| 可靠执行 | 持久化任务、租约、重试、取消、多数据库、随机假值脱敏与回填 | [架构](docs/agent-architecture.md) |
| 开发集成 | HTTP API、审批转发、可续传 SSE、可靠事件消费者 | [事件](docs/events.md) · [消费者](docs/event-consumers.md) |

<details>
<summary><strong>展开完整能力与当前边界</strong></summary>

- 任务创建、去重、查询、取消、失败重试和每次执行的历史。
- SQLite 原子领取、租约续期、过期接管和旧 Worker 写入拦截；存储经 `TaskStore` trait 抽象，SQLite 为默认实现，另提供 PostgreSQL 适配器（`--store postgres`）。
- 常驻 Worker / 单次执行，超时、退避重试和 SIGINT/SIGTERM 停止。
- Codex CLI 子进程：stdin 输入、JSONL 事件、受限大小的日志、结构化结果校验。
- 可选 Pi JSONL RPC 底座：默认关闭，启用后与 Codex 共用 Worker、租约、重试和脱敏管线；首版同样只支持 `analysis_v1`，见 [Pi 底座](docs/agent-pi.md)。
- 独立尝试目录；Unix 上取消/超时清理子进程组，其他平台仅保证直接子进程终止。
- 通用底座注册、能力声明、任务按后端路由，以及后端/模型/会话记录。
- 任务与审核的持久化事件日志、可续传 SSE，以及带租约、确认和退避重试的可靠事件消费者；见 [事件](docs/events.md)与[事件消费者](docs/event-consumers.md)。
- QQ 官方机器人消息网关：群@/单聊消息幂等入队为任务，任务到终态后被动回复原会话；内置「帮助/任务/任务 id/取消 id/绑定 口令」，并通过模块命令支持「精读 arxiv-id/水电」。
- 飞书自定义群机器人 webhook：任务终态经 `[notify]` 默认出口主动推送到群；`ru41 notify` 可独立运行通知派发。
- arXiv 论文追踪：`papers poll/run` 定时抓取新论文，按 arXiv ID 去重、关键词粗筛后合成每日精选 digest 任务交 Codex 分析，终态推送飞书/QQ；QQ 可按 ID 发起单篇精读。
- RSS/Atom 信息流追踪：`rss poll/run` 轮询订阅源，同一套去重/粗筛/digest/推送管线，输出「发生了什么→为什么重要→与我有关→下一步」四段式精选。
- 校园卡水电监控：`ecard poll/run` 每日查询宿舍水电余额（ecard 平台），读数落库形成历史，低于阈值经 `[notify]` 出口提醒；协议见 [水电监控说明](docs/ecard-waterfee.md)。
- Agent 调用前信息脱敏：prompt 中的手机号、身份证、邮箱、银行卡号、姓名替换为随机同类假数据，返回结果回填真值后落库；映射只留宿主内存，日志仅记录脱敏规则计数，`worker --no-sanitize` 可关闭。
- 旧版数据库自动迁移；自动行为测试与 GitHub Actions。

当前真实底座为 **Codex** 与可选 **Pi**；其他 Agent 不会因接口存在而自动可用。两个底座目前都不提供会话续接或外部写入工具，模拟后端仅用于验证公共接口与路由。

</details>

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

Worker 会实际调用 Codex，使用已有认证并消耗对应额度。适配器使用 `--ignore-user-config`、`--sandbox read-only` 和独立尝试目录；不自动加载用户原生 Codex 配置；自定义供应商与启动覆盖通过 Ru41 配置显式传入。可通过 `--model` 显式选择账户可用模型。只读沙箱不是独立机器或完整数据隔离。

<details>
<summary><strong>常驻运行、取消与重试语义</strong></summary>

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

</details>

## Docker 部署

仓库提供多阶段 `Dockerfile` 和 `deploy/compose.yaml`：一个镜像分别运行 worker、QQ、通知和各来源模块，SQLite、runs 与配置均挂载宿主机目录。最小启动方式：

```bash
cp -n ru41.example.toml ru41.toml
docker build -t ru41:local .
export RU41_CODEX_HOME="$HOME/.codex"
docker compose -f deploy/compose.yaml up -d worker
```

附属通过 Compose profiles 按需启动，`./deploy/backup.sh` 使用 SQLite 在线备份。完整配置、认证、备份恢复和重依赖扩展见 [Docker 部署指南](deploy/README.md)。**Docker 只解决部署与自动重启，不等于断电保护；仍需持久卷、备份和异地副本。**

## QQ 消息网关

<details>
<summary><strong>QQ 启动命令、回复行为与运行数据</strong></summary>

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
- 任务进入 `succeeded`/`failed` 后，网关把结果摘要或错误作为**被动回复**发回原群/原单聊，并记录 `notified_at` 避免正常工作与并发派发时重复发送；发送成功但登记前崩溃的极端窗口仍可能重发，见核心状态说明。
- QQ 被动回复要求带原消息的 `msg_id` 且约 5 分钟内有效；超过窗口或重启较晚的任务可能回复失败（日志可见，任务结果不受影响）。QQ 对主动推送有配额与频控限制，本网关以被动回复为主。协议细节与限制见 [QQ 渠道说明](docs/channels-qq.md)。

运行数据默认位于 `.ru41/state.db` 和 `.ru41/runs/`，可以分别通过 `--db`、`--runs-dir` 指定。不要将运行数据放进 Vault。日志包含任务内容及模型输出，需要按私人数据管理；当前没有自动清理策略。

</details>

## 附属模块：论文、RSS 与水电

<details>
<summary><strong>论文与 RSS 配置示例、轮询命令及水电接入</strong></summary>

三个业务模块都通过 TOML 配置和 `bootstrap/assembly.rs` 静态装配，不需要修改 Worker。最小论文/RSS 配置示例：

```toml
[notify]
channel = "feishu"

[channel.feishu]
webhook_url = "https://open.feishu.cn/open-apis/bot/v2/hook/<token>"

[sources.arxiv]
categories = ["cs.CR", "cs.AI"]
keywords = ["agent", "llm security"]
interests = "AI 安全方向，关注 LLM 与 Agent 安全"

[sources.rss]
feeds = ["https://example.com/feed.xml"]
interests = "AI 安全、Rust 与个人自动化"
```

`poll` 执行一轮，`run` 常驻周期执行；论文/RSS 的 digest 任务仍需独立 Worker 完成，终态通知由 `qq run` 内嵌派发循环或独立的 `ru41 notify` 进程发出：

```bash
cargo run --locked -- papers poll     # 或 papers run
cargo run --locked -- rss poll        # 或 rss run
cargo run --locked -- worker          # 执行 digest / 精读任务
cargo run --locked -- notify          # 未运行 QQ 网关时的独立通知进程
```

水电模块使用 `[sources.ecard]`，必填平台地址、token 和房间参数；密钥建议经 `RU41_ECARD_TOKEN` 注入，完整字段见 `ru41.example.toml`。详细说明：[arXiv](docs/sources-arxiv.md)、[RSS](docs/sources-rss.md)、[水电](docs/ecard-waterfee.md)、[飞书通知](docs/channels-feishu.md)。

</details>

## 配置文件

除 CLI 参数外，也可以用 TOML 配置文件。加载顺序：`--config <路径>` > `./ru41.toml` > `~/.config/ru41/config.toml`；都没有则用内置默认值。同一配置的取值优先级为 **CLI 参数 > 环境变量 > 配置文件 > 默认值**。`ru41.toml` 已在 `.gitignore` 中（密钥不要进 git），样例见 `ru41.example.toml`：

```toml
[store]           # kind = "sqlite"|"postgres"、path、url
[worker]          # timeout_secs、runs_dir、sanitize
[agent.codex]     # bin、model、provider
[agent.pi]        # enabled、bin、model、provider；默认不启用
[channel.qq]      # app_id、app_secret、bind_secret、default_notify_target
[channel.feishu]  # webhook_url；建议改用 RU41_FEISHU_WEBHOOK_URL
[notify]          # channel = "feishu" 或 "qq"
[sources.arxiv]   # categories、keywords、interests、notify 等
[sources.rss]     # feeds、keywords、interests、notify 等
[sources.ecard]   # base_url、token、房间参数、阈值、notify 等
[api]             # listen、token；令牌建议用 RU41_API_TOKEN
```

使用 PostgreSQL 作为存储时，通过 `--store postgres --pg-url`（或环境变量 `RU41_PG_URL`、配置 `store.url`）指定连接串，启动时自动执行 `migrations/postgres/` 下的迁移：

```bash
export RU41_PG_URL="postgres://user:pass@localhost:5432/ru41"
cargo run --locked -- --store postgres task list
cargo run --locked -- --store postgres worker --once
```

## 核心架构

```text
HTTP API / CLI / QQ 网关 / 定时来源
                  ↓
   应用服务与业务模块（TaskService / interactions / digest）
                  ↓
     TaskStore（SQLite 默认，可选 PostgreSQL）+ 运行/交互/投递记录
                  ↓
       Worker：租约、重试、恢复；按任务 backend_id 路由
                  ↓
       AgentBackend 通用接口（ports）
                  ↓
   Codex exec / Codex app-server / Pi JSONL RPC（均为 analysis_v1）
                  ↓
   结果校验与事务保存；交互请求持久化等待用户决定
                  ↓
   NotifyDispatcher：独立投递记录，按渠道派发（QQ / 飞书）
```

核心概念分离：**Task 是要完成的工作，AgentRun 是一次底座运行（含状态与 session），Interaction 是底座发出的审批/补充信息请求**。一个任务可多次运行；底座运行成功不等于任务成功。审批由底座触发、Ru41 转发：交互记录持久化到数据库，当前可通过 HTTP API 回复；回复校验选项、有效期与运行状态后经适配器回传底座。QQ 审批命令尚未开放，避免在核心恢复语义完成前提供不可靠入口。

任务与审核状态变化还会原子写入事件日志，供 SSE 与可靠消费者订阅；现有 QQ/飞书通知仍走 NotifyDispatcher，尚未迁移到事件消费者。

脱敏在 Agent 边界完成：prompt 出站前替换为随机同类假值，映射只保留在宿主内存，日志只记录规则计数，**不落入 Agent 可访问的运行目录**；结果回填真值后落库。

```text
src/
├── domain/           # 任务、尝试、运行、交互与投递的数据与规则
├── app/              # 公共用例：TaskService、InteractionService、digest、身份绑定
├── modules/          # 垂直业务附属：papers、rss、ecard（配置/来源/业务/QQ 命令各自收拢）
├── channels/         # 消息渠道：qq/（机器人、协议、命令）、feishu.rs（webhook 通知）
├── ports/            # 可替换能力接口：AgentBackend、TaskStore、NotifyChannel、InteractionHandler
├── runtime/          # 长驻执行机制：Worker、NotifyDispatcher
├── privacy/          # 脱敏与回填
├── adapters/
│   ├── agent/            # 底座实现：codex/（exec 与 app-server）、pi/（RPC）
│   ├── database/         # 存储实现：SQLite、PostgreSQL
│   └── sources/          # 跨模块通用来源协议（RSS/Atom feed）
├── ingress/
│   ├── http/             # HTTP API（axum，/api/v1，Bearer 认证，SSE 事件流）
│   ├── cli.rs            # CLI 命令
│   └── schedule.rs       # 通用定时轮询入口
├── bootstrap/        # 配置聚合与附属装配（assembly.rs）
├── util/             # 少量无业务规则的工具
└── main.rs           # 进程启动与命令分发
```

职责约束：`app` 只描述“做什么”；`runtime` 负责“如何持续执行”；`ports` 定义可替换边界；`adapters` 翻译外部协议，不决定业务成功或审核通过；所有入口（HTTP/CLI/QQ）调用同一套应用服务。

### 附属模块如何接入

业务代码按功能放在 `src/modules/<name>/`，消息渠道放在 `src/channels/`；核心不反向依赖业务模块。当前采用**编译期静态装配**，不是放入目录就自动加载的插件系统：`bootstrap/config.rs` 聚合配置，`bootstrap/assembly.rs` 构造 tracker、注册 QQ 命令与通知渠道，`main.rs` 负责命令分发。新增模块的目录约定、注册清单和测试要求见 [附属模块开发指南](docs/module-development.md)。

## 自定义模型 API

<details>
<summary><strong>自定义供应商配置与参数说明</strong></summary>

底座默认用你本机的 Codex 登录；也可以让 codex 走任意 OpenAI 兼容端点（OpenRouter、DeepSeek、本地 vLLM 等）：

```toml
# ru41.toml
[agent.codex]
model = "deepseek-chat"

[agent.codex.provider]
id = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"   # export DEEPSEEK_API_KEY=sk-...
wire_api = "chat"
```

实现方式是 codex 官方的 `-c model_providers.<id>={...}` 与 `-c model_provider=<id>` 启动覆盖，`exec` 与 `app-server`（会话+审批）两种模式都生效。密钥只经环境变量由 codex 子进程自读，宿主与配置文件不接触密钥值。参数由 `adapters/agent/codex/provider.rs` 校验并生成。

</details>

## Pi 底座（可选）

<details>
<summary><strong>启用 Pi 并创建任务</strong></summary>

Pi 默认不注册，认证与自定义提供方由 Pi 原生配置管理。启用后可按 `backend = "pi"` 创建任务，与 Codex 共用同一个 Worker：

```toml
[agent.pi]
enabled = true
bin = "pi"
# model = "提供方/模型ID"
# provider = "提供方"  # 与 model 同时指定
```

```bash
cargo run --locked -- backends
cargo run --locked -- task add --backend pi --key pi:demo:1 --prompt "总结给定材料"
cargo run --locked -- worker --once
```

Pi 首版不支持工具、会话恢复或审批；协议边界与验证范围见 [Pi 底座](docs/agent-pi.md)。

</details>

## HTTP API

```bash
export RU41_API_TOKEN=$(openssl rand -hex 24)   # 至少 24 字节
cargo run --locked -- serve --listen 127.0.0.1:4141

curl -H "Authorization: Bearer $RU41_API_TOKEN" http://127.0.0.1:4141/api/v1/tasks
curl -X POST -H "Authorization: Bearer $RU41_API_TOKEN" -H "Content-Type: application/json" \
  -d '{"key":"api:1","prompt":"给个人论文阅读计划列出三个建议","backend":"codex"}' \
  http://127.0.0.1:4141/api/v1/tasks
curl -X POST -H "Authorization: Bearer $RU41_API_TOKEN" -H "Content-Type: application/json" \
  -d '{"decision":"accept"}' \
  http://127.0.0.1:4141/api/v1/interactions/{id}/reply
```

端点：`GET/POST /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`POST /api/v1/tasks/{id}/cancel|retry`、`GET /api/v1/backends`、`GET /api/v1/runs`、`GET /api/v1/interactions`、`POST /api/v1/interactions/{id}/reply`、`GET /api/v1/events`（SSE）、`GET /api/v1/consumers/{name}`、`POST /api/v1/consumers/{name}/retry`、`GET /api/v1/openapi.json`。API 与 CLI、QQ 共用同一套应用服务与存储；任务由独立 worker 进程执行，serve 只做入口。事件游标与消费者恢复语义见 [事件文档](docs/events.md)和[消费者文档](docs/event-consumers.md)。

## 文档导航

- 产品与规划：[产品设计](docs/product-design.md)、[开发路线](docs/roadmap.md)、[核心状态](docs/core-status.md)
- 渠道：[QQ](docs/channels-qq.md)、[飞书](docs/channels-feishu.md)
- 来源与业务：[arXiv](docs/sources-arxiv.md)、[RSS](docs/sources-rss.md)、[水电监控](docs/ecard-waterfee.md)
- Agent 与运行时：[底座架构](docs/agent-architecture.md)、[底座接入](docs/agent-backends.md)、[Pi](docs/agent-pi.md)、[事件](docs/events.md)、[可靠消费者](docs/event-consumers.md)
- 扩展开发：[附属模块开发指南](docs/module-development.md)；全部文档见 [docs 索引](docs/README.md)

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

自动测试使用模拟 Agent 或本地假进程，不调用付费模型、不访问真实 Vault。覆盖数据库升级、并发领取、租约接管、取消竞态、重试预算、协议错误、日志上限、进程清理与多底座路由。
