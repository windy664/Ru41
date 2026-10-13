# Ru41

基于 **Rust + Codex** 的个人自动化助手，围绕 Obsidian 组织个人想法、一手信息和任务，面向学习、科研、项目与校园生活。

> 当前是 **v0.1 开发起点**：产品设计、核心代码结构、持久化任务 CLI 和 Codex 环境检查。任务暂时只保存为 `queued`，没有执行 Worker，不会自动调用模型、读取或修改你的 Vault。

## 已实现

- Rust 单 package，按 domain / app / adapters / ingress 分层。
- SQLite 保存任务，提供创建、列表、详情和取消命令。
- 相同请求键与内容重复提交返回已有任务；同键不同内容明确报错。
- 已取消任务不会因重复投递重新排队。
- 数据库迁移版本检查、事务与并发去重。
- `doctor` 检查本地 Codex CLI 版本，不验证登录、不调用模型。
- 生命周期、进程重开后的持久化、并发投递和版本兼容测试；GitHub Actions 检查格式、Clippy 与测试。

## 快速开始

需要支持 Rust 2024 edition 的 Rust 工具链；本地验证使用 Rust 1.98.0。仅运行 `doctor` 需要已安装 Codex CLI。

```bash
git clone https://github.com/windy664/Ru41.git
cd Ru41
cargo build --locked
cargo run --locked -- --help
cargo run --locked -- doctor
```

创建和查看任务（目前不会执行任务）：

```bash
cargo run --locked -- task add --key demo:inbox:1 --prompt "整理 Inbox 中的临时想法"
cargo run --locked -- task list
cargo run --locked -- task show 1
cargo run --locked -- task cancel 1
```

默认数据库为当前目录下 `.ru41/state.db`，可用全局参数指定：

```bash
cargo run --locked -- --db /path/to/runtime/state.db task list
```

`--key` 标识一次输入事件：同一次投递重试使用相同键，新任务使用新键。CLI 输出 JSON，出错返回非零退出码。列表默认返回最近 20 条，可用 `--limit` 调整（1–1000）。

运行数据放在独立目录，避免放进 Obsidian Vault。数据库、认证信息和私人笔记不应提交到代码仓库。

## 核心架构

```text
CLI / Git 更新 / 定时采集 / 消息网关
                     ↓
             TaskService + SQLite
                     ↓
              Worker（计划中）
                     ↓
          Rust 操作 / Codex Agent
                     ↓
           校验 → 自动执行或审核
                     ↓
          Obsidian / 外部动作 / 回复
```

所有入口共用任务系统；Codex 负责理解和任务内推理；Rust 负责持久状态、工具、权限、写入和结果核验。先验证核心与两个真实流程，再设计模块注册和插件机制。

```text
src/
├── domain/         # 任务类型与状态
├── app/            # TaskService
├── adapters/       # SQLite、Codex CLI 环境检查
├── ingress/        # CLI
├── lib.rs
└── main.rs
migrations/         # 数据库迁移
tests/             # 行为测试
docs/              # 产品设计与开发路线
```

## 下一步

1. Worker、任务尝试记录、租约及中断恢复。
2. Codex 运行接入、结构化结果、超时和执行记录。
3. Git 更新触发 Inbox 整理、链接修复和可恢复归档。
4. 论文/RSS 一手信息进入同一 Obsidian 知识库。
5. QQ、飞书或钉钉先完成一个双向消息入口。
6. 核心稳定后再提炼模块与插件接口。

详细说明：[产品设计](docs/product-design.md) · [开发路线](docs/roadmap.md)。设计文档描述目标能力，实际实现范围以本 README 为准。

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

测试不调用模型或访问真实 Vault。
