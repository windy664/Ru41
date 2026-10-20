# 使用与配置手册

[← 返回项目首页](../README.md) · [文档索引](README.md)

任务操作、部署、配置与接口的完整示例集中在这里。

## 快速开始

### 01 · 准备环境

需要支持 Rust 2024 edition 的工具链；本地验证使用 Rust 1.98.0、Codex CLI 0.163.0，Docker 镜像当前内置 0.162.1。默认 OpenAI 底座需提前完成 Codex 登录，自定义供应商则使用对应 API Key。

```bash
git clone https://github.com/windy664/Ru41.git
cd Ru41
cargo build --locked
cargo run --locked -- init --non-interactive
cargo run --locked -- doctor
cargo run --locked -- backends
```

`init` 幂等创建最小配置、数据目录和数据库，不覆盖已有配置，也不接收密钥。`doctor` 默认只做离线检查；`doctor --deep` 才会检查现有数据库并真实调用模型。`backends` 列出当前编译注册的底座和支持的契约。

### 02 · 执行第一个任务

创建分析任务、运行一次 Worker，再查看结果与执行历史：

```bash
cargo run --locked -- task add --key demo:1 --backend codex --prompt "给个人论文阅读计划列出三个建议"
cargo run --locked -- worker --once
cargo run --locked -- task show 1
cargo run --locked -- task attempts 1
```

Worker 会实际调用 Codex，使用已有认证并消耗对应额度。适配器使用 `--ignore-user-config`、`--dangerously-bypass-approvals-and-sandbox` 和独立尝试目录；不自动加载用户原生 Codex 配置；自定义供应商与启动覆盖通过 Ru41 配置显式传入。可通过 `--model` 显式选择账户可用模型。yolo 模式会无审批执行命令，只应部署在已经外部隔离的环境中。

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

## 运维与诊断

```bash
# 配置路径、脱敏内容与纯配置校验
cargo run --locked -- config path
cargo run --locked -- config show
cargo run --locked -- config check

# 离线环境检查；--deep 才检查现有数据库并调用模型
cargo run --locked -- doctor
cargo run --locked -- doctor --deep

# 任务计数、启用组件与脱敏排障报告
cargo run --locked -- status
cargo run --locked -- dump

# QQ 官方扫码创建/绑定机器人，再检查凭证与 gateway
cargo run --locked -- qq onboard
cargo run --locked -- qq doctor  # 不连接 WebSocket、不发送消息
```

`qq onboard` 使用官方机器人开通/绑定流程，不是个人 QQ 登录。扫码成功后仅把 AppID/ClientSecret 原子写入权限为 `0600` 的本地配置；其他配置与注释保留，Secret 不进入日志或 `dump`。随后单聊可直接提问，群聊需要 @机器人。

`config show` 和 `dump` 会隐藏密钥、连接串、webhook、通知目标、个人房间字段及自由 overrides；分享前仍应人工复核。`status` 的六种任务计数在数据库内聚合，不会把全部 prompt 或结果读入宿主。检查类命令发现错误时返回非零退出码，可用于部署脚本。

## Docker 部署

**一个镜像，按需启动不同进程。**

| 进程角色 | 职责 |
| :--- | :--- |
| Worker | 领取并执行 Agent 任务 |
| QQ 网关 | 接收消息、入队与回复 |
| 通知派发 | 将任务结果发送到配置的渠道 |
| 来源模块 | 周期轮询论文、RSS 或水电数据 |

仓库提供多阶段 `Dockerfile` 和 `deploy/compose.yaml`：一个镜像分别运行 worker、QQ、通知和各来源模块，SQLite、runs 与配置均挂载宿主机目录。最小启动方式：

```bash
cp -n ru41.example.toml ru41.toml
docker build -t ru41:local .
export RU41_CODEX_HOME="$HOME/.codex"
docker compose -f deploy/compose.yaml up -d worker
```

附属通过 Compose profiles 按需启动，`./deploy/backup.sh` 使用 SQLite 在线备份。完整配置、认证、备份恢复和重依赖扩展见 [Docker 部署指南](../deploy/README.md)。**Docker 只解决部署与自动重启，不等于断电保护；仍需持久卷、备份和异地副本。**

## QQ 消息网关

**从聊天发起任务，在原会话接收结果。**

| 入口 | 使用方式 |
| :--- | :--- |
| 群聊 / 单聊 | @机器人或直接发送文本，创建分析任务 |
| 任务管理 | `帮助` · `任务` · `任务 id` · `取消 id` |
| 业务命令 | `精读 arxiv-id` · `水电` · `待办` · `记待办 [时间] <内容>` · `日程` · `记日程 <时间> <标题>` |

[渠道接入说明 →](channels-qq.md)

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
- QQ 被动回复要求带原消息的 `msg_id` 且约 5 分钟内有效；超过窗口或重启较晚的任务可能回复失败（日志可见，任务结果不受影响）。QQ 对主动推送有配额与频控限制，本网关以被动回复为主。协议细节与限制见 [QQ 渠道说明](channels-qq.md)。

运行数据默认位于 `.ru41/state.db` 和 `.ru41/runs/`，可以分别通过 `--db`、`--runs-dir` 指定。不要将运行数据放进 Vault。日志包含任务内容及模型输出，需要按私人数据管理；当前没有自动清理策略。

</details>

## 私人待办

Todo 是长期个人行动项，不等同于 Agent 执行队列里的 `Task`。数据持久化在同一数据库的通用 `todo_item` 记录中；“今日、近期、长期、逾期”由读取时的本地日期计算，不把会过期的分类写死到数据里。

```bash
# 添加；日期支持 YYYY-MM-DD、YYYY-MM-DD HH:MM、今天、明天、后天、N天后
cargo run --locked -- todo add --title "写项目周报" --due "明天下午3点" --label project,weekly
# 摘要视图：逾期、今日、近期、长期、收件箱
cargo run --locked -- todo list
# 单视图
cargo run --locked -- todo list overdue
cargo run --locked -- todo list today
cargo run --locked -- todo list long-term
# 完成 / 推迟 / 取消 / 恢复；ID 可用完整短 ID 或不歧义前缀
cargo run --locked -- todo done <id>
cargo run --locked -- todo postpone <id> "2026-10-21 18:00"
cargo run --locked -- todo cancel <id>
cargo run --locked -- todo restore <id>
# 主动提醒：--once 适合 cron；常驻进程缺省每分钟检查一次
cargo run --locked -- todo remind --once --channel qq --target "user:<openid>"
cargo run --locked -- todo remind --channel qq --target "user:<openid>" --interval-secs 60
```

QQ 单聊中已绑定身份后也可以：

```text
/记待办 明天下午3点 写项目周报
/待办
/待办 逾期
/完成 <待办ID>
/推迟 <待办ID> 到 后天晚上8点
/取消待办 <待办ID>
/记日程 明天14:00-16:00 班会
/记日程 每周二14:00-15:40 体育课
/日程
/取消日程 <日程ID>
```

列表类回复带快捷按钮（点击 = 代发对应指令，如「完成 xx」「取消日程 xx」），平台不支持键盘时自动降级为纯文本。`/待办` 摘要与 `/日程` 共用今日视图：行程是时间轴上的占用（起止、地点、每周重复），
与待办（任务闭环）概念分离，但到点提醒由同一个 `todo remind` 循环发送。CLI 侧对应
`ru41 event add/list/cancel/conflicts`，`conflicts` 供 agent 规划前做占用检查。

`[todo]` 可配置本地时区固定偏移、近期窗口和长期窗口：

```toml
[todo]
utc_offset_minutes = 480
recent_days = 7
long_term_days = 14
```

设置 `--due` 时默认在同一时间提醒，也可用 `--remind` 另设提醒时间；`/推迟` 会把截止和提醒一起改到新时间并重新触发。`todo remind` 到点后经 `--channel/--target` 或 `[notify]` 默认出口发送，通知成功后才记录去重；网络失败会在下一轮重试。循环待办和论文/RSS 自动写入待办在后续接入。

## 附属模块：论文、RSS、水电与待办

| 模块 | 输入 → 输出 | 运行入口 |
| :--- | :--- | :--- |
| [论文追踪](sources-arxiv.md) | arXiv 分类与关键词 → 精选摘要 / 单篇精读 | `papers poll/run` |
| [信息订阅](sources-rss.md) | RSS / Atom → 与个人兴趣相关的摘要 | `rss poll/run` |
| [宿舍水电](ecard-waterfee.md) | 校园卡平台 → 余额历史与阈值提醒 | `ecard poll/run` |
| [智慧树作业提醒](sources-zhihuishu.md) | sidecar 报告 → 新作业 / 临近截止 / 考试式提醒 | `zhihuishu poll/run` |
| [学习通签到监控](sources-chaoxing.md) | 协议端直连 → 待签活动提醒 / 手动签到 | `chaoxing poll/run/sign` |
| Obsidian Vault 闭环 | Inbox Git 新提交 → Agent 归档整理 → 提交推送（核心服务 `app::obsidian` + `ports::vault`，附属仅 QQ 命令与 skill） | `obsidian poll/run` |
| 博客发布管线 | Vault `Pending-待发布/` 目录 → 校验 → 博客仓库 `_posts/` → 推送 Pages | `blog poll/run` |
| 私人待办 | 今日、近期、长期与逾期行动项 → 持久化 Todo 列表 | `todo ...` / QQ `/待办` |
| 行程（日程事件） | 课表、会议、约会等时间占用（一次性 / 每周重复）→ 展开、冲突检测、到点提醒 | `event ...` / QQ `/日程` |

来源采集、Agent 执行、私人待办与通知派发各自运行，通过持久化数据衔接。

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

`poll` 执行一轮，`run` 常驻周期执行；论文/RSS 的 digest 任务仍需独立 Worker 完成，终态通知由 `qq run` 内嵌派发循环或独立的 `ru41 notify` 进程发出。所有附属的轮询任务都登记在定时任务注册表里，可用 `schedule list` 查看任务名、间隔与启停状态，用 `schedule run <任务名>` 统一启动（等价于对应模块的 `run`）：

```bash
cargo run --locked -- schedule list           # 全部已登记任务及 enabled 状态
cargo run --locked -- schedule run papers     # 等价于 papers run
cargo run --locked -- papers poll     # 或 papers run
cargo run --locked -- rss poll        # 或 rss run
cargo run --locked -- worker          # 执行 digest / 精读任务
cargo run --locked -- notify          # 未运行 QQ 网关时的独立通知进程
```

水电模块使用 `[sources.ecard]`，必填平台地址、token 和房间参数；密钥建议经 `RU41_ECARD_TOKEN` 注入，完整字段见 `ru41.example.toml`。详细说明：[arXiv](sources-arxiv.md)、[RSS](sources-rss.md)、[水电](ecard-waterfee.md)、[飞书通知](channels-feishu.md)。

</details>

## 配置文件

从 [ru41.example.toml](../ru41.example.toml) 开始配置；本地 `ru41.toml` 已被 Git 忽略，密钥通过环境变量注入。

| 配置规则 | 优先级（从高到低） |
| :--- | :--- |
| 文件选择 | `--config <路径>` → `./ru41.toml` → `~/.config/ru41/config.toml` |
| 字段取值 | CLI 参数 → 环境变量 → 配置文件 → 默认值 |

没有配置文件时使用内置默认值。

<details>
<summary><strong>配置分区速查</strong></summary>

```toml
[store]           # kind = "sqlite"|"postgres"、path、url
[worker]          # timeout_secs、runs_dir、sanitize
[agent.codex]     # bin、model、output_schema、config_overrides、provider
[agent.pi]        # enabled、bin、model、provider；默认不启用
[channel.qq]      # app_id、app_secret、bind_secret、default_notify_target
[channel.feishu]  # webhook_url；建议改用 RU41_FEISHU_WEBHOOK_URL
[notify]          # channel = "feishu" 或 "qq"
[sources.arxiv]   # categories、keywords、interests、notify 等
[sources.rss]     # feeds、keywords、interests、notify 等
[sources.ecard]   # base_url、token、房间参数、阈值、notify 等
[api]             # listen、token；令牌建议用 RU41_API_TOKEN
```

</details>

### 存储选择

| SQLite · 默认 | PostgreSQL · 可选 |
| :--- | :--- |
| 本地文件，适合单机部署 | 通过连接串接入数据库服务 |
| `.ru41/state.db` | `RU41_PG_URL` / `store.url` |

使用 PostgreSQL 作为存储时，通过 `--store postgres --pg-url`（或环境变量 `RU41_PG_URL`、配置 `store.url`）指定连接串，启动时自动执行 `migrations/postgres/` 下的迁移：

```bash
export RU41_PG_URL="postgres://user:pass@localhost:5432/ru41"
cargo run --locked -- --store postgres task list
cargo run --locked -- --store postgres worker --once
```

## 核心架构

Ru41 管理执行生命周期，Agent 适配器负责对接底座协议。

![Ru41 核心架构：入口与应用服务将任务持久化，Worker 经过脱敏调用 Codex 或 Pi，校验回填后保存结果，再通过通知渠道与持久化事件对外提供结果。](assets/architecture.png)

### 任务、运行与交互

| 概念 | 表达什么 | 边界 |
| :--- | :--- | :--- |
| `Task` | 用户希望完成的工作 | 一项任务可以有多次运行 |
| `AgentRun` | 一次底座运行及其状态、会话 | 底座运行成功不等于任务成功 |
| `Interaction` | 底座发出的审批 / 补充信息请求 | 由 Ru41 转发与持久化，用户通过 HTTP API 回复 |


审批由底座触发、Ru41 转发：交互记录持久化到数据库，当前可通过 HTTP API 回复；回复校验选项、有效期与运行状态后经适配器回传底座。QQ 审批命令尚未开放，避免在核心恢复语义完成前提供不可靠入口。

任务与审核状态变化还会原子写入事件日志，供 SSE 与可靠消费者订阅；现有 QQ/飞书通知仍走 NotifyDispatcher，尚未迁移到事件消费者。

脱敏在 Agent 边界完成：prompt 出站前替换为随机同类假值，映射只保留在宿主内存，日志只记录规则计数，**不落入 Agent 可访问的运行目录**；结果回填真值后落库。

### 代码布局

<details>
<summary><strong>展开目录与职责</strong></summary>

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

</details>

| 层 | 核心职责 |
| :--- | :--- |
| `app` | 描述用例；所有入口复用同一套应用服务 |
| `runtime` | 持续执行、调度与通知派发 |
| `ports` | 定义可替换能力边界 |
| `adapters` | 翻译外部协议，不决定业务成功或审核通过 |

### 附属模块如何接入

业务代码按功能放在 `src/modules/<name>/`，消息渠道放在 `src/channels/`；核心不反向依赖业务模块。当前采用**编译期静态装配**，不是放入目录就自动加载的插件系统：`bootstrap/config.rs` 聚合配置，`bootstrap/assembly.rs` 构造 tracker、注册 QQ 命令与通知渠道，`main.rs` 负责命令分发。新增模块的目录约定、注册清单和测试要求见 [附属模块开发指南](module-development.md)。

## 自定义模型 API

通过 Ru41 配置向 Codex 传入供应商设置，API Key 由子进程从环境变量读取。

<details>
<summary><strong>自定义供应商配置：MiMo 示例与参数说明</strong></summary>

底座默认用你本机的 Codex 登录；也可以让 codex 走 OpenAI Responses 兼容端点。下面是 MiMo 的完整示例（API Key 通过环境变量注入，不写入配置）：

```toml
# ru41.toml
[agent.codex]
model = "mimo-v2.6-pro"
output_schema = false  # 使用 Codex 原生文本回复，Ru41 映射为 summary 并校验
config_overrides = [
  'model_catalog_json="/absolute/path/model-catalogs.json"',
  'model_reasoning_effort="high"',
  'model_reasoning_summary="none"',
  'model_context_window=1048576',
  'web_search="disabled"',
]

[agent.codex.provider]
id = "mimo"
base_url = "https://api.xiaomimimo.com/v1"
api_key_env = "MIMO_API_KEY"   # export MIMO_API_KEY=sk-...
wire_api = "responses"
```

MiMo 的 `model-catalogs.json` 与参数说明见[官方 Codex 配置指南](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/codex-configuration)。`config_overrides` 可传递其他 Codex `-c key=value` 配置，路径应使用绝对路径。

实现方式是 codex 官方的 `-c model_providers.<id>={...}` 与 `-c model_provider=<id>` 启动覆盖，`exec` 与 `app-server`（会话+审批）两种模式都生效。密钥只经环境变量由 codex 子进程自读，宿主与配置文件不接触密钥值。参数由 `adapters/agent/codex/provider.rs` 校验并生成。Codex 0.163 起只支持 `responses`；`chat` 仅供旧版 CLI 使用。

</details>

## Pi 底座（可选）

**同一套 Worker，按任务选择底座。** Pi 默认关闭，启用后可使用 `backend = "pi"` 路由。

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

Pi 首版不支持工具、会话恢复或审批；协议边界与验证范围见 [Pi 底座](agent-pi.md)。

</details>

## HTTP API

**服务入口与任务执行分离。** `serve` 提供 API，独立 Worker 负责执行任务；API、CLI 与 QQ 共用应用服务和存储。

### 启动与调用

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

### 接口速查

所有路径均以 `/api/v1` 为前缀。

| 资源 | 方法与路径 | 用途 |
| :--- | :--- | :--- |
| 任务 | `GET /tasks` · `POST /tasks` · `GET /tasks/{id}` | 创建、列表与详情 |
| 任务控制 | `POST /tasks/{id}/cancel` · `POST /tasks/{id}/retry` | 取消与重试 |
| 底座与运行 | `GET /backends` · `GET /runs` | 查询底座及运行记录 |
| 交互 | `GET /interactions` · `POST /interactions/{id}/reply` | 查询与回复底座请求 |
| 事件流 | `GET /events` | 可续传 SSE |
| 消费者 | `GET /consumers/{name}` · `POST /consumers/{name}/retry` | 消费状态与重试 |
| 接口规范 | `GET /openapi.json` | OpenAPI 文档 |

[事件与游标 →](events.md) · [可靠消费者与恢复 →](event-consumers.md)

## 文档导航

| 我想…… | 从这里开始 |
| :--- | :--- |
| 了解产品方向与进度 | [产品设计](product-design.md) · [路线图](roadmap.md) · [核心状态](core-status.md) |
| 部署到自己的服务器 | [Docker 部署](../deploy/README.md) · [配置样例](../ru41.example.toml) |
| 接入消息渠道 | [QQ](channels-qq.md) · [飞书](channels-feishu.md) |
| 配置个人信息来源 | [arXiv](sources-arxiv.md) · [RSS](sources-rss.md) · [水电](ecard-waterfee.md) |
| 增加一个 Agent 底座 | [底座架构](agent-architecture.md) · [接入契约](agent-backends.md) · [Pi](agent-pi.md) |
| 订阅事件或开发新模块 | [事件](events.md) · [消费者](event-consumers.md) · [模块开发](module-development.md) |

[浏览全部文档 →](README.md)

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

自动测试使用模拟 Agent 或本地假进程，不调用付费模型、不访问真实 Vault。覆盖数据库升级、并发领取、租约接管、取消竞态、重试预算、协议错误、日志上限、进程清理与多底座路由。
