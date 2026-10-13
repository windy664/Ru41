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
- 旧版数据库自动迁移；20 个行为测试与 GitHub Actions。

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

运行数据默认位于 `.ru41/state.db` 和 `.ru41/runs/`，可以分别通过 `--db`、`--runs-dir` 指定。不要将运行数据放进 Vault。日志包含任务内容及模型输出，需要按私人数据管理；当前没有自动清理策略。

使用 PostgreSQL 作为存储时，通过 `--store postgres --pg-url`（或环境变量 `RU41_PG_URL`）指定连接串，启动时自动执行 `migrations/postgres/` 下的迁移：

```bash
cargo run --locked -- --store postgres --pg-url postgres://user:pass@localhost:5432/ru41 task list
cargo run --locked -- --store postgres worker --once
```

## 核心架构

```text
CLI / 后续 Git、定时器与消息入口
                  ↓
        TaskService + TaskStore（默认 SQLite 实现，可选 PostgreSQL）
                  ↓
       Worker：租约、重试、恢复
                  ↓
       AgentBackend 通用接口
                  ↓
       CodexAdapter（首个实现）
                  ↓
        结果校验与事务保存
```

任务绑定 `backend_id` 与版本化 `analysis_v1` 契约。Worker 只领取对应底座、兼容契约的任务；失败时不自动换底座。`session_id` 是该底座内部的不透明标识，不能跨底座续接。

```text
src/
├── domain/           # 任务、尝试、结果与执行契约
├── app/
│   ├── task_service.rs
│   ├── agent_service.rs  # AgentBackend + BackendRegistry
│   ├── store.rs          # TaskStore 存储接口（SQLite 为默认实现，可选 PostgreSQL）
│   └── worker.rs
├── adapters/         # SQLite、PostgreSQL 与 Codex 协议
├── ingress/          # CLI
├── lib.rs
└── main.rs           # 注册并装配具体底座
```

接口接入规则见 [Agent 底座契约](docs/agent-backends.md)。完整目标见 [产品设计](docs/product-design.md)，实现进度见 [开发路线](docs/roadmap.md)。

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

自动测试使用模拟 Agent 或本地假进程，不调用付费模型、不访问真实 Vault。覆盖数据库升级、并发领取、租约接管、取消竞态、重试预算、协议错误、日志上限、进程清理与多底座路由。
