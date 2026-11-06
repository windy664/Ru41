# 工程约定

这份是**协作契约**：目录边界、构建纪律、部署分层、红线。改代码前先扫一眼，
改完按这里的规矩自检。上手路径见[开发者上手](onboarding.md)，插件机制细节见
[动态插件开发指南](plugin-development.md)。

## 1. 目录边界（核心 vs 附属）

```
src/           核心：domain/ ports/ app/ runtime/ privacy/ adapters/ ingress/http/
               （src/modules/ 已清空成注册表空壳；跨附属数据契约住 domain/，
                 如 domain/timetable.rs 课表快照）
plugins/<名>/  附属插件，独立 crate（各自 Cargo.toml + Cargo.lock）
docs/          设计文档（plugin-development.md、core-status.md、sources-*.md …）
deploy/        Dockerfile / 部署脚本
migrations/    数据库迁移（核心领地）
```

- 附属开发**只加插件**，不改 `src/` 的公共 trait、Worker、数据库实现；
  改公共接口必须同步全部调用方与测试。
- 共享协调点（改动前先想清楚波及面）：`src/main.rs`、
  `src/bootstrap/config.rs`、各 `mod.rs`、根 `Cargo.toml`、`migrations/`。

## 2. agent 底座（统一入口）

- **只有一个入口**：`[agent].default_backend` 决定全局默认底座。
  QQ 入口可用 `--backend` 临时覆盖；CLI `ru41 task add` 不写 `--backend`
  就用这个默认值。
- **供应商只在一处定义**：`[agent.pi]` 的 `base_url` / `api` / `api_key_env` /
  `model`。Obsidian 的 AI 合并复用同一份配置与同一套协议。
- 换底座不用改核心：`AgentBackend` 抽象（契约、描述符、结果与用量口径一致）
  不变，底座配置在配置文件的底座段里。
- 密钥只经环境变量注入子进程，配置文件里只有变量名。
- 踩坑：走 OpenAI 风格工具历史的底座，对 Anthropic 协议端点会报
  `tool call result does not follow tool call`；原生支持 Anthropic 协议的
  底座没这个问题。

## 3. 构建与测试

提交前本地先过一遍 CI 查的两条，省一轮镜像构建：

```bash
cargo fmt --all -- --check                 # CI 的 fmt 检查
cargo clippy --all-targets -- -D warnings  # CI 的 clippy 检查
cargo test                                 # 核心测试
cd plugins/<名> && cargo test              # 插件各测各的（不在 workspace）
```

- 主 crate 是**唯一 workspace 成员**：`cargo build` / `cargo test` 只覆盖
  `src/` + `tests/`。
- 插件**不在 workspace 里**，`cargo test -p <名>` 找不到包；进插件目录单独跑。
- 插件共享 target：`plugins/.cargo/config.toml` 把 target-dir 指到
  `plugins/target-plugins/` 并启用 lld 链接器——依赖只编一次，全插件复用。
  **别在插件目录 `cargo clean`**，会把所有插件的共享产物一起清掉。
- 插件依赖主 crate 走 path；`ru41::ports::vault::Vault` 等公共 trait 变更会让
  所有插件的测试 fake 同步失编译（`sync` 返回 `SyncOutcome`，记得有
  `resolve_conflict`/`rebase_continue`/`rebase_abort`）。
- 插件一切异步入口（PollJob::poll_json / QQ handler / CLI poll）必须过
  `ru41_plugin_api::bridge`——dylib 各静态链接一份 tokio，宿主 reactor 对
  插件不可见，不过桥在网络调用时 panic 并 abort 进程。核心 tracker 类型被
  插件 tracker 闭包实例化进 .so 时同样适用。
- 插件技能包在 `plugins/<名>/skills/<技能>/SKILL.md`，由 `src/skills/` 加载
  注入 agent prompt。容器内从镜像的 `/usr/share/ru41/{modules,plugins}/` 读
  （`/etc/ru41` 被宿主机 bind mount 整体遮蔽，技能包放那里等于没有）。
- `plugins/*/Cargo.lock` 会被本地 `cargo test` 改动，提交前用
  `git checkout -- plugins/*/Cargo.lock` 复原。

## 4. 部署链：先分层，再选路

| 改了什么 | 走哪条路 | 耗时 |
|---|---|---|
| 只有 `plugins/<名>/**`（代码或 skills/） | **热更**：`deploy/hot-patch.sh <插件名> [容器名...]` | ~35 秒 |
| 动了 `src/**`、`crates/**`、`migrations/**`、根 `Cargo.toml` | **完整链**：push → CI → 搬镜像 → roll | 10-12 分钟 |
| 只动 `docs/**` 或 `*.md` | 不用部署（CI 也会跳过） | 0 |

判定方法：`git diff --name-only HEAD~1`——**只要出现 `src/`、`crates/`、
`migrations/`、根 `Cargo.toml`，就只能走完整链**。热更脚本会自己检查，
发现核心改动就拒绝执行（除非 `--force`）。

完整链：

1. commit + `git push origin main main:exp/plugin-api`。
2. CI（单一 workflow，check 与 image 两个 job **并行**）构建
   `ghcr.io/windy664/ru41:<40位SHA>`。
3. 本机 `docker pull` → `docker save | ssh <server> docker load`。
4. 服务器：`docker tag ...:SHA ru41:local` → `docker create` + `docker cp`
   抽插件 → 滚动重建（自动发现全部 ru41 容器，名字前缀 `ru41-` 且 entrypoint
   是 ru41，无手写名单）。
5. 验证：容器数量、逐容器日志。

### 两条踩过的坑（别再踩）

- **先部署插件，再改配置**：插件配置段是 `deny_unknown_fields`，插件还不认识
  新字段时，容器会**解析失败起不来**（表现为容器崩溃循环）。
  顺序永远是：部署新插件 → 再写配置 → 重启容器。
- **别批量 retry 历史失败任务**：那等于让机器人补跑几天前的活，还会给人发
  一堆过期通知。要重跑就只挑**当次**真正失败的那几条。

## 5. Vault 双向同步（PR 模型）

本地与服务器都会改 vault，直接共用一个分支必然打架：本地推 master 被拒，
rebase 会把冲突标记插进笔记正文，而本地没有 AI 合并能力（密钥只在服务器）。

成熟做法是 PR 模型（同构于 GitHub PR）：

- **本地**：编辑在自己的分支上，脚本只做「commit → 推自己的分支 → 取回
  master」。**本地永远不做 merge/rebase/abort**，所以不会有冲突标记、
  不会停摆。
- **服务器**：合并队列检测远端分支，merge 进 master，冲突走已有的 AI 语义
  合并，合完删除待合并分支。用 merge 不用 rebase——保留提交 SHA，历史都在。
- 合并用的是底座供应商同一份配置；直连端点时模型名要去掉供应方前缀。

坑：

- **容器里的 git 凭据**：全局 gitconfig 的 `credential.helper = store` 指向
  bind mount 的凭据文件时，git 改写它会 `EBUSY`（rename 覆盖挂载点），
  **所有 fetch 全挂**。统一用 `store --file=<普通文件>`，全局配置里先写空
  `helper =` 清掉默认列表再加回。
- **合并用的容器要自己带 key 和代理**：它跑 AI 合并，密钥与出网代理原本
  只在 worker 容器里。
- **`.obsidian/workspace.json` 必须 gitignore**：每次开 Obsidian 都变，入 git
  只会制造无谓提交与冲突。
- 服务器聚合文件里的时间戳（如「最近巡查」）每轮都变 → 每轮必提交 →
  无谓冲突面。

## 6. 红线

- 配置文件、运行目录、任何 key/cookie/密码**不进 git、不进博客、不进 Issue**；
  写文档用占位符。
- vault（Obsidian）仓库的自提交一律 `[ru41]` 前缀，不要伪装成人工提交。
- QQ/飞书消息渲染走渠道网关，插件不要自己直连渠道 API。
- 机器生成的状态/镜像数据不进 vault：用户看的是知识与索引，不是我们的内部
  快照（要状态就给命令，不要写文件）。

## 7. 文档入口

- 上手路径：[开发者上手](onboarding.md)
- 插件机制：[动态插件开发指南](plugin-development.md)
- 核心状态：[核心宿主状态](core-status.md)
- 命令/配置/HTTP API 全集：[usage.md](usage.md)
- 各数据源逆向笔记与协议红线：`docs/sources-*.md`、`docs/*-recon.md`
- 渠道协议细节：[channels-qq.md](channels-qq.md) / [channels-feishu.md](channels-feishu.md)
