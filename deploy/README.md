# Docker 部署

Ru41 使用一个镜像运行多个进程。镜像包含 Ru41、Codex CLI 和 sqlite3；**不包含 Pi 或无头浏览器**。这些重依赖以后按独立边车或专用镜像增加，不影响主镜像。

## 1. 准备配置与数据目录

在项目根目录执行：

```bash
cp -n ru41.example.toml ru41.toml
mkdir -p .ru41/codex .ru41/backups
docker build -t ru41:local .
```

按需要编辑 `ru41.toml`：Codex、QQ/飞书、通知出口和各 sources 配置仍与非 Docker 部署相同。真实配置、数据库和认证目录都被 `.gitignore` 与 `.dockerignore` 排除，不会进入镜像。

容器内统一使用：

| 宿主机路径 | 容器路径 | 内容 |
|---|---|---|
| `ru41.toml` | `/etc/ru41/ru41.toml`（只读） | 本地配置 |
| `.ru41/state.db` | `/data/state.db` | SQLite 任务与 records 数据 |
| `.ru41/runs/` | `/data/runs/` | Agent 运行目录 |
| `.ru41/codex/` | `/home/node/.codex/` | 默认的容器专用 Codex 认证目录 |
| `.ru41/backups/` | `/data/backups/` | 数据库备份 |

## 2. 配置 Codex 并启动 Worker

复用宿主机已有的 Codex 登录：

```bash
export RU41_CODEX_HOME="$HOME/.codex"
docker compose -f deploy/compose.yaml up -d worker
```

该目录以可写方式挂载，因为 CLI 可能刷新认证信息；其中含有敏感凭据，应按私密数据管理。

也可以不设置 `RU41_CODEX_HOME`，使用容器专用目录登录：

```bash
docker compose -f deploy/compose.yaml run --rm --entrypoint codex worker login
docker compose -f deploy/compose.yaml up -d worker
```

不调用模型的检查：

```bash
docker compose -f deploy/compose.yaml run --rm worker doctor
docker compose -f deploy/compose.yaml run --rm worker backends
```

Worker 会实际执行队列任务并消耗模型额度，首次启动前请确认数据库与配置正确。

## 3. 按需启动附属

附属服务使用 Compose profiles，未配置的功能不会因 `docker compose up` 被全部拉起。

```bash
# QQ 网关（内含通知派发，不要再同时运行 notify）
docker compose -f deploy/compose.yaml --profile qq up -d worker qq

# 飞书等独立通知进程
docker compose -f deploy/compose.yaml --profile notify up -d worker notify

# arXiv + 独立通知
docker compose -f deploy/compose.yaml --profile papers --profile notify up -d worker papers notify

# RSS + 独立通知
docker compose -f deploy/compose.yaml --profile rss --profile notify up -d worker rss notify

# 水电监控（纯 Rust 阈值判断，不要求 worker）
docker compose -f deploy/compose.yaml --profile ecard up -d ecard

# 恒源云 GPU 库存（Cookie 走环境变量；需 [sources.compshare] 与默认 QQ 出口）
RU41_COMPSHARE_COOKIE='完整的 .compshare.cn Cookie' \
  docker compose -f deploy/compose.yaml --profile compshare up -d compshare

# 教务课表（CAS 自动登录、变更提醒、明日课表、日程同步；需先配置 [sources.jwxt]）
docker compose -f deploy/compose.yaml --profile jwxt up -d jwxt

# 私人待办到点提醒（需要设置 QQ 单聊目标）
RU41_TODO_NOTIFY_TARGET="user:<openid>" docker compose -f deploy/compose.yaml --profile todo up -d todo

# HTTP API，仅绑定宿主机 127.0.0.1:4141
docker compose -f deploy/compose.yaml --profile api up -d serve worker
```

`papers/rss/ecard/compshare` 缺少必填配置时会直接报错，应先完成对应配置再启用 profile。`qq run` 与独立 `notify` 二选一，避免同时运行两条通知派发路径。

常用运维：

```bash
docker compose -f deploy/compose.yaml ps
docker compose -f deploy/compose.yaml logs -f worker
docker compose -f deploy/compose.yaml down
```

升级时先重新构建，再用原来的 profile 组合启动。Compose 会为所有服务设置 `init: true` 和 `restart: unless-stopped`，但自动重启不等于数据备份。

镜像内置 `TZ=Asia/Shanghai`（见 Dockerfile）：课表日程、每日提醒、日期边界都按本地时区计算。容器若跑在 UTC，所有时刻会差 8 小时，且宿主与容器会互相覆盖对方的日程数据。改时区只需改 Dockerfile 里的 `TZ` 与 `/etc/localtime`。

## 4. 备份与恢复

保持 worker 运行，执行在线备份：

```bash
./deploy/backup.sh
```

脚本使用 SQLite `.backup`，而不是直接复制写入中的数据库文件，输出 `.ru41/backups/state-<UTC>.db`。`ru41.toml`、`.ru41/runs/` 和 `.ru41/codex/` 需要另行归档；**本机备份不能防止整机或磁盘损坏，应再同步到其他机器**。

恢复前必须停止全部写入服务：

```bash
docker compose -f deploy/compose.yaml down
cp .ru41/state.db .ru41/state.db.before-restore
cp .ru41/backups/state-<timestamp>.db .ru41/state.db
rm -f .ru41/state.db-wal .ru41/state.db-shm
# 按原 profile 组合重新启动
docker compose -f deploy/compose.yaml up -d worker
```

不要在任何 Ru41 服务仍写数据库时替换文件。Docker 的 restart 与 Worker 的租约恢复只能恢复进程和任务执行，不能替代 UPS、文件系统落盘、在线备份和异地副本。

## 5. 后续重依赖扩展

首版 Compose 不预置浏览器。某个附属确实需要无头浏览器时，优先增加只供内网访问的 browser sidecar，由该模块通过 CDP/WebSocket/HTTP 调用；不要让核心 Worker 安装浏览器，也不要给 Ru41 挂载 Docker socket。需要 Python 等其他运行时同理，使用独立辅助服务而不是膨胀主镜像。
