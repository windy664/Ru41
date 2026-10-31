# AGENTS.md — ru41 仓库协作契约

给在这个工作区里工作的 agent（任何底座：Codex / Kimi Code / Claude Code 等）的入门指令。
**架构、命令、约定变化时，顺手更新本文件** —— 这就是本仓库的「活记忆」，不要让它腐烂。

## 项目是什么

ru41 是一个 Rust 个人助理运行时：核心任务调度器（SQLite/Postgres + Worker + 渠道网关）驱动 agent 底座（Codex CLI 等）执行定时/交互任务，附属功能全部以**外部插件（.so）**挂载。

## 目录边界（核心 vs 附属）

```
src/           核心：domain/ ports/ app/ runtime/ privacy/ adapters/ ingress/http/
plugins/<名>/  附属插件，独立 crate（各自 Cargo.toml + Cargo.lock）
docs/          设计文档（plugin-development.md、core-status.md、sources-*.md …）
deploy/        Dockerfile / 部署脚本
migrations/    数据库迁移（核心领地）
```

- 附属开发**只加插件**，不改 `src/` 的公共 trait、Worker、数据库实现；改公共接口必须同步全部调用方与测试。
- 共享协调点（改动前先想清楚波及面）：`src/main.rs`、`src/bootstrap/config.rs`、各 `mod.rs`、根 `Cargo.toml`、`migrations/`。

## 构建与测试（容易踩坑）

- 主 crate 是**唯一 workspace 成员**：`cargo build` / `cargo test` 只覆盖 `src/` + `tests/`。
- 插件**不在 workspace 里**，`cargo test -p <名>` 找不到包；进插件目录单独跑：`cd plugins/zhihuishu && cargo test`。
- 插件依赖主 crate 走 path；`ru41::ports::vault::Vault` 等公共 trait 变更会让所有插件的测试 fake 同步失编译（`sync` 返回 `SyncOutcome`，记得有 `resolve_conflict`/`rebase_continue`/`rebase_abort`）。
- 插件技能包在 `plugins/<名>/skills/<技能>/SKILL.md`，由 `src/skills/` 加载注入 agent prompt。

## 部署链（生产机 backup238 / 192.168.1.223，目录 /home/windy2/ru41-mimo-smoke）

1. commit + `git push origin main main:exp/plugin-api`。
2. CI 的 `image` workflow 构建 `ghcr.io/windy664/ru41:<40位SHA>`（约 10 分钟，后台轮询 `gh run view <id>`）。
3. 本机 `docker pull` → `docker save | ssh windy2@192.168.1.223 docker load`。
4. 服务器：`docker tag ...:SHA ru41:local` → `docker create` + `docker cp CID:/etc/ru41/plugins/. ~/ru41-mimo-smoke/plugins/` + `docker rm` → `python3 ~/ru41-roll.py && python3 ~/ru41-roll-3.py`（滚动重启全部 ru41-* 容器，自动备份到 ~/roll-backup-*）。
5. 验证：`docker ps | grep -c Up`（应 ~27）、`docker logs ru41-<插件> --since 5m`。

## 红线

- `ru41.toml`、`.ru41/`、任何 key/cookie/密码**不进 git、不进博客、不进 Issue**；写文档用占位符。
- vault（Obsidian）仓库的自提交一律 `[ru41]` 前缀，不要伪装成人工提交。
- QQ/飞书消息渲染走渠道网关，插件不要自己直连渠道 API。

## 文档入口

- 插件开发：`docs/plugin-development.md`；核心状态：`docs/core-status.md`
- 各数据源逆向笔记：`docs/*-recon.md`；渠道：`docs/channels-qq.md` / `channels-feishu.md`
