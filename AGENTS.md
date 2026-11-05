# AGENTS.md — ru41 仓库协作契约

给在这个工作区里工作的 agent（任何底座：Codex / Kimi Code / Claude Code 等）的入门指令。
**架构、命令、约定变化时，顺手更新本文件** —— 这就是本仓库的「活记忆」，不要让它腐烂。

## 项目是什么

ru41 是一个 Rust 个人助理运行时：核心任务调度器（SQLite/Postgres + Worker + 渠道网关）驱动 agent 底座（Codex CLI 等）执行定时/交互任务，附属功能全部以**外部插件（.so）**挂载。

## 目录边界（核心 vs 附属）

```
src/           核心：domain/ ports/ app/ runtime/ privacy/ adapters/ ingress/http/
               （src/modules/ 已于 2026-11-03 全部插件化清空，只剩注册表空壳；
                 跨附属数据契约住 domain/，如 domain/timetable.rs 课表快照）
plugins/<名>/  附属插件，独立 crate（各自 Cargo.toml + Cargo.lock）
docs/          设计文档（plugin-development.md、core-status.md、sources-*.md …）
deploy/        Dockerfile / 部署脚本
migrations/    数据库迁移（核心领地）
```

- 附属开发**只加插件**，不改 `src/` 的公共 trait、Worker、数据库实现；改公共接口必须同步全部调用方与测试。
- 共享协调点（改动前先想清楚波及面）：`src/main.rs`、`src/bootstrap/config.rs`、各 `mod.rs`、根 `Cargo.toml`、`migrations/`。

## agent 底座（codex / pi 并存）

- 两个底座共用 `AgentBackend` 抽象（契约、描述符、结果与用量口径一致），配置在 `[agent.codex]` 与 `[agent.pi]`。
- **切底座不用改核心**：QQ 入口的底座由容器命令参数 `--backend` 决定（`ru41 ... qq run --backend pi`）；
  worker 侧只要 `[agent.pi]` 配好模型、provider 与密钥变量名。
- 镜像内置两个 CLI：`@openai/codex` 与 `@earendil-works/pi-coding-agent`（Dockerfile 顶部 ARG 钉版本）。
- 密钥只经环境变量注入子进程，配置文件里只有变量名（codex 用 `env_key`，pi 用 `api_key_env` + `models.json` 的 `$NAME` 引用）。
- 踩坑：codex 走 OpenAI 风格工具历史，MiniMax 这类 Anthropic 协议端点会报
  `tool call result does not follow tool call`；pi 原生支持 Anthropic 协议，是这类端点的适配器。

## 构建与测试（容易踩坑）

- 主 crate 是**唯一 workspace 成员**：`cargo build` / `cargo test` 只覆盖 `src/` + `tests/`。
- 插件**不在 workspace 里**，`cargo test -p <名>` 找不到包；进插件目录单独跑：`cd plugins/zhihuishu && cargo test`。
- 插件共享 target：`plugins/.cargo/config.toml` 把 target-dir 指到
  `plugins/target-plugins/` 并启用 lld 链接器——ru41 依赖只编一次，全插件复用。
  **别在插件目录 `cargo clean`**，会把所有插件的共享产物一起清掉。
  本地产物路径：`plugins/target-plugins/release/libru41_plugin_<名>.so`（scp 部署用这条）。
- 插件依赖主 crate 走 path；`ru41::ports::vault::Vault` 等公共 trait 变更会让所有插件的测试 fake 同步失编译（`sync` 返回 `SyncOutcome`，记得有 `resolve_conflict`/`rebase_continue`/`rebase_abort`）。
- 插件一切异步入口（PollJob::poll_json / QQ handler / CLI poll）必须过 `ru41_plugin_api::bridge`——dylib 各静态链接一份 tokio，宿主 reactor 对插件不可见，不过桥在网络调用时 panic 并 abort 进程。**核心 tracker 类型被插件 tracker 闭包实例化进 .so 时同样适用**（2026-11-04 ClassRemindTracker 未过桥致 class-remind 容器崩溃循环、课前提醒全丢）。
- 插件技能包在 `plugins/<名>/skills/<技能>/SKILL.md`，由 `src/skills/` 加载注入 agent prompt。
  容器内从镜像的 `/usr/share/ru41/{modules,plugins}/` 读（`/etc/ru41` 被宿主机
  bind mount 整体遮蔽，技能包放那里等于没有）。**2026-11-05 起技能包也改为宿主目录
  bind mount**：生产机 `~/ru41-mimo-smoke/skills` 挂到 `/usr/share/ru41/plugins`，
  目录形状必须是 `<插件>/skills/<技能>/SKILL.md`（加载器按这个结构找），
  热更脚本会连带同步，改技能不必再重建镜像。

## 部署链（生产机 backup238 / 192.168.1.223，目录 /home/windy2/ru41-mimo-smoke）

### 先分层，再选路（默认热更，别默认 CI）

| 改了什么 | 走哪条路 | 耗时 |
|---|---|---|
| 只有 `plugins/<名>/**`（代码或 skills/） | **热更**：`deploy/hot-patch.sh <插件名> [容器名...]` | **~35 秒** |
| 动了 `src/**`、`crates/**`、`migrations/**`、根 `Cargo.toml` | **完整链**：push → CI → 搬镜像 → roll | 10-12 分钟 |
| 只动 `docs/**` 或 `*.md` | 不用部署（CI 也会跳过） | 0 |

判定方法：`git diff --name-only HEAD~1`（或 `git status`）——**只要出现 `src/`、`crates/`、
`migrations/`、根 `Cargo.toml`，就只能走完整链**。热更脚本会自己检查，发现核心改动就拒绝执行
（除非 `--force`）。

### 热更（只改插件时，约 35 秒）

`deploy/hot-patch.sh <插件名> [容器名...]`
——容器内 `cargo build --release --features plugin-entry`（与镜像同一工具链）→ scp .so + 技能包 →
`docker restart` 目标容器。插件是 cdylib，容器启动时从 `/etc/ru41/plugins` 加载，替换 .so 重启即生效。

热更**不覆盖**：宿主二进制变了（插件 ABI 对不上）、要重建 sidecar。

### 完整链（核心代码 / 出正式镜像）

1. commit + `git push origin main main:exp/plugin-api`。
2. CI（单一 workflow，check 与 image 两个 job **并行**）构建 `ghcr.io/windy664/ru41:<40位SHA>`（约 5-6 分钟，后台轮询 `gh run view <id>`）。
3. 本机 `docker pull` → `docker save | ssh windy2@192.168.1.223 docker load`（镜像约 2.7GB，这一步 100-140 秒）。
4. 服务器：`docker tag ...:SHA ru41:local` → `docker create` + `docker cp CID:/etc/ru41/plugins/. ~/ru41-mimo-smoke/plugins/` + `docker rm` → `bash ~/ru41-roll.sh`（自动发现全部 ru41 容器——名字前缀 `ru41-` 且 entrypoint 是 ru41，无手写名单；滚动重建到 `ru41:local`，自动备份到 ~/roll-backup-*；仓库同源副本在 `deploy/roll.sh`）。
5. 验证：`docker ps | grep -c Up`（应 ~32）、`docker logs ru41-<插件> --since 5m`。

### 两条踩过的坑（别再踩）

- **先部署插件，再改配置**：插件配置段是 `deny_unknown_fields`，插件还不认识新字段时，
  容器会**解析失败起不来**（2026-11-05 在 lecture / ecard 上各踩一次，表现为容器崩溃循环）。
  顺序永远是：部署新插件 → 再写配置 → 重启容器。
- **别批量 retry 历史失败任务**：那等于让机器人补跑几天前的活，还会给你发一堆过期通知。
  要重跑就只挑**当次**真正失败的那几条（2026-11-05 误 retry 39 个历史任务，已取消收场）。

## 红线

- `ru41.toml`、`.ru41/`、任何 key/cookie/密码**不进 git、不进博客、不进 Issue**；写文档用占位符。
- vault（Obsidian）仓库的自提交一律 `[ru41]` 前缀，不要伪装成人工提交。
- QQ/飞书消息渲染走渠道网关，插件不要自己直连渠道 API。

## 文档入口

- 插件开发：`docs/plugin-development.md`；核心状态：`docs/core-status.md`
- 各数据源逆向笔记：`docs/*-recon.md`；渠道：`docs/channels-qq.md` / `channels-feishu.md`
