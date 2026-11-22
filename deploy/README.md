# Docker 部署

Ru41 使用一个镜像运行多个进程。镜像包含 Ru41、Codex CLI、Pi、全部插件、技能包和 sqlite3/git/ffmpeg 等基础工具；**不包含你的配置、密钥、业务数据、vault、博客仓库或 ASR 模型**。这些重依赖按独立边车或专用镜像增加，不影响主镜像。

## 1. 准备配置与数据目录

在项目根目录执行：

```bash
cp -n ru41.example.toml ru41.toml
mkdir -p .ru41/codex .ru41/backups plugins.d
docker build -t ru41:local .
```

按需要编辑 `ru41.toml`：Codex/Pi、QQ/飞书、通知出口和各核心配置仍与非 Docker 部署相同。插件配置拆到 `plugins.d/<插件>/config.toml`（所有容器只读挂载），插件凭证只走环境变量、不写进配置目录。真实配置、数据库、插件本地配置和认证目录都被 `.gitignore` 与 `.dockerignore` 排除，不会进入镜像。

裸机部署可以设置 `RU41_HOME` 统一布局：`$RU41_HOME/ru41.toml`、`$RU41_HOME/plugins.d/`、`$RU41_HOME/data/`。Docker 容器内仍以 `/etc/ru41` 与 `/data` 为准，`RU41_HOME` 不是必需项。

容器内统一使用：

| 宿主机路径 | 容器路径 | 内容 |
|---|---|---|
| `ru41.toml` | `/etc/ru41/ru41.toml`（只读） | 本地主配置 |
| `plugins.d/` | `/etc/ru41/plugins.d`（只读，worker 不挂载） | 插件自有配置与设备本地覆盖 |
| `.ru41/state.db` | `/data/state.db` | SQLite 任务与 records 数据 |
| `.ru41/runs/` | `/data/runs/` | Agent 运行目录 |
| `.ru41/codex/` | `/home/node/.codex/` | 默认的容器专用 Codex 认证目录 |
| `.ru41/backups/` | `/data/backups/` | 数据库备份 |

## 2. 配置 Codex 并启动 Worker

Worker 与其他容器**共用同一份 `ru41.toml`**：插件配置已全部搬到 `plugins.d/<插件>/config.toml`，
凭证只经环境变量注入插件自己的容器，因此主配置里没有任何插件密钥，agent 直接挂主配置即可
（2026-11-17 起不再生成 `ru41-worker.toml` 裁剪副本）。

插件的非敏感配置放 `plugins.d/`，凭证放环境变量：

```bash
# 插件配置示例（plugins.d/jwxt/config.toml，段名 = 插件配置键）
# [jwxt]
# semester_start = "2026-08-31"
# section_times = ["09:00", "09:40", ...]

# 凭证只走环境变量，注入真正需要登录的插件容器
docker run --rm -e RU41_JWXT_USERNAME -e RU41_JWXT_PASSWORD ...
```

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

**插件先行**：papers/rss/ecard/competitions 等业务附属是 cdylib 动态插件，
compose 已把 `../.ru41/plugins/` 挂到 `/etc/ru41/plugins`，但要先构建插件：

```bash
./deploy/plugins-build-all.sh   # 净室编译全部插件（glibc 与镜像一致）
mkdir -p .ru41/plugins
cp plugins/*/target-docker/release/*.so .ru41/plugins/
```

插件与宿主的握手规则（api 版本 + 依赖 stamp）见
[插件开发指南](../docs/plugin-development.md)。

**插件构建的两个坑**（2026-11-15 实测踩过，构建脚本已修）：

- `plugins/.cargo/config.toml` 是本地开发加速配置（clang + lld 链接器），**宿主有 clang 但构建镜像 `rust:1.98-trixie` 只有 cc**。cargo 的配置发现基于 cwd，从 `plugins/<name>` 目录构建会读到它 → `linker 'clang' not found`。构建脚本改用 `--manifest-path` 从构建根目录发起，避开这份本地配置。
- 插件共享 target（`plugins/.target-docker-shared`）是容器以 root 创建的，宿主侧 `git clean` 删不动 → 权限报错直接中断脚本。清理命令已排除该目录（`-e .target-docker-shared`）。

**核心改动上线：插件 .so 必须一起刷新。** 插件是 cdylib，但它调用的是
`ru41` 这个 lib crate 的函数（如 `app::obsidian::ObsidianService`、
`adapters::vault::GitVault`），**这些代码会被静态链接进 .so**。所以只换宿主
二进制、.so 留在旧版本，插件侧跑的还是旧逻辑——现象是"镜像更新了、行为没变"。
生产发布链：

```bash
# 1. 本地出镜像并推到 GHCR（CI 也会推 ghcr.io/windy664/ru41:latest）
docker build -t ghcr.io/windy664/ru41:latest .
docker push ghcr.io/windy664/ru41:latest

# 2. 服务器换镜像
docker pull ghcr.io/windy664/ru41:latest
docker save ghcr.io/windy664/ru41:latest | ssh <server> 'docker load'

# 3. 滚动重建 + 补回环境变量
#    roll.sh 滚动前自动完成：registry 镜像 tag 成 ru41:local +
#    把镜像里的插件 .so 和技能包同步到宿主挂载目录（关键：容器挂的是宿主目录，
#    RU41_SYNC_PLUGINS=0 可关）。这两步不再需要手工做。
ssh <server> 'RU41_IMAGE=ghcr.io/windy664/ru41:latest bash ~/ru41-roll.sh \
  && bash ~/roll-set-env.sh <容器名> <参考容器名> <KEY...>'
```

> ⚠️ **不要在生产机上编译**（2026-11-22 实证：这台宿舍服务器 i3-8130U / 15W / 无电池，
> CPU 满载会把适配器保护顶掉 → 整机断电，日志都来不及写；取证见
> `deploy/ru41-power-watch.sh` 的说明与 `~/.local/state/ru41-power/incidents.log`）。
> 在生产机上跑 `docker build` / `cargo build` 时长一点就可能把机器顶掉 ——
> 那天就是这样断电的。**构建一律在本地做，只往生产机传产物**（二进制、插件 .so、
> 镜像 tar），生产机上只做「拷贝 + docker load + roll」。
>
> ⚠️ 补丁镜像（`Dockerfile.patch`）**必须带插件**：`roll.sh` 会用镜像里的插件覆盖
> 宿主挂载目录，补丁镜像若不带 `.plugins-dist/`，宿主上热更过的 .so 会被回退成
> 旧版（2026-11-22 实测：rss 插件被回退后不认配置里的新字段 → 容器崩溃循环）。
> `build-fast.sh` 已经会先把插件编进 `.plugins-dist/`（`RU41_SKIP_PLUGINS=1` 可跳过）。

> ⚠️ `roll-set-env.sh` 的用法是 **三个参数**：`<目标容器> <参考容器> <KEY...>`——
> 要补的变量值**从参考容器里抄**（不必把密钥写进命令行）。
> 它会按 inspect 把目标容器**原有的环境变量全量恢复**，再追加补的那些。
> 早期版本这里写错过（`select(startswith(...)) | not` 把非目标变量全过滤掉，
> 加上 jq 少个右括号直接编译失败 → 进程替换里的失败不触发 `set -e` →
> 重建出的容器只剩镜像默认环境，密钥/代理/bot token 全丢且**没有任何报错**，
> 2026-11-17 夜里把 worker 和 telegram 弄挂过）。现在它会自查：
> 备份里有 env、恢复出来却没有 → 直接中止，不重建（`891d6a8`）。

插件同步漏掉时，日志里会出现"插件与宿主 git commit 不同（插件 xxx / 宿主 yyy）"
的提示——那是 .so 比宿主旧，不是握手失败，行为上却等同于没升级。

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

# 竞赛日程总览 / 实习岗位总览 / 项目计划总览 / 科研机会总览（写 vault，需 RU41_VAULT_PATH）
RU41_VAULT_PATH=<vault 克隆目录> docker compose -f deploy/compose.yaml \
  --profile competitions --profile jobs --profile programs --profile research up -d

# 作业聚合 / 项目卡片 / 页面监控
docker compose -f deploy/compose.yaml --profile homework --profile projects --profile pagewatch up -d

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

## 4. 备份、换机迁移与恢复

### 日常数据库备份

保持 worker 运行，执行在线备份：

```bash
./deploy/backup.sh
```

脚本使用 SQLite `.backup`，而不是直接复制写入中的数据库文件，输出 `.ru41/backups/state-<UTC>.db`。**本机备份不能防止整机或磁盘损坏，应再同步到其他机器**。

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

### 换机迁移包

裸机或需要同时迁移配置时，使用 `config export`：

```bash
ru41 config export --out /path/to/ru41-migration
```

目标目录必须尚不存在。导出结构为：

```text
ru41-migration/
├── manifest.toml
├── config/
│   ├── ru41.toml
│   └── plugins.d/<插件>/{config.toml,config.local.toml}
├── data/state.db
├── secrets.required.toml
└── versions/{ru41.txt,plugin-api.txt,plugins.txt}
```

- SQLite 通过在线一致性快照导出，不需要停止 worker；配置密钥与密码管理器中的凭据真值会被剔除，`secrets.required.toml` 只记录需要重新注入的环境变量名。
- **state.db 仍包含任务、行程、身份绑定等私密数据，整个迁移包都必须按私密文件保护**，不要上传到 GitHub、Issue 或公共网盘。
- PostgreSQL 的数据在外部服务中，不进入迁移包；Vault 与博客仓库继续走各自 Git；插件 `.so`、runs、知识向量库、模型缓存、登录态、浏览器/ASR sidecar 和 Compose 启停清单不打包。

把迁移包安全复制到新机器后，先预检：

```bash
ru41 config restore \
  --from /path/to/ru41-migration \
  --target /home/<user>/ru41
```

默认 dry-run 不写文件，会检查迁移格式、`config_version`、plugin-api/ABI、SQLite 完整性和 schema 版本，并列出当前缺少的环境变量。确认后执行：

```bash
ru41 config restore \
  --from /path/to/ru41-migration \
  --target /home/<user>/ru41 \
  --apply
export RU41_HOME=/home/<user>/ru41
ru41 doctor
```

`--apply` 只接受尚不存在的目标目录，绝不覆盖现有配置或数据库；恢复时会把 SQLite 与 runs 路径重写为 `$RU41_HOME/data/`。随后按报告重新注入环境变量、按需重建凭据管理器记录，并单独克隆 Vault/博客仓库、安装匹配版本的插件和 sidecar。

## 5. 重依赖与 sidecar

首版 Compose 不预置浏览器。某个附属确实需要无头浏览器时，优先增加只供内网访问的 browser sidecar，由该模块通过 CDP/WebSocket/HTTP 调用；不要让核心 Worker 安装浏览器，也不要给 Ru41 挂载 Docker socket。需要 Python 等其他运行时同理，使用独立辅助服务而不是膨胀主镜像。

现有 sidecar 实例（与 Ru41 只用一个 JSON 文件解耦）：

| Sidecar | 运行时 | 产出 |
|---|---|---|
| [`sidecar/boss-crawler/`](../sidecar/boss-crawler/) | Python + DrissionPage + Chromium | `boss_jobs.json` → jobs 插件 |
| [`sidecar/zhihuishu-sidecar/`](../sidecar/zhihuishu-sidecar/) | 上游 Autovisor（Playwright + Xvfb） | `report.json` → zhihuishu 插件；另含**视频挂机**（刷课）与作业采集器 |
| compose `browser` profile | browserless/chromium | JS 重度页面的统一渲染出口 |

**sidecar 镜像也走 CI**（`.github/workflows/sidecar-image.yml`，与主仓 `ci.yml` 并行）：上游源码按钉死的 sha 拉取、套 `patches/` 再构建，推 `ghcr.io/windy664/zhihuishu-sidecar`。主仓的 `ci.yml` 用 `paths-ignore` 排除了 `sidecar/**`，所以 sidecar 走单独一条 workflow。

### 5.1 生产机自更新（人不在家也能更新）

CI 出镜像只解决了「一半」：生产机在家里 LAN、无公网入口，GitHub Actions 连不上它。
更新链路的最后一环由**生产机自己**完成——`deploy/auto-update.sh`：

```
laptop push → CI check(绿) → 推 GHCR → 生产机 timer(每 30 分钟)拉镜像 → 比对 → roll.sh → 健康检查 → 不健康自动回滚
```

```bash
# 装（用户级单元；生产机 ~/ru41-mimo-smoke/deploy/ 下要有脚本）
cp deploy/auto-update.{service,timer} ~/.config/systemd/user/
cp deploy/auto-update.sh ~/ru41-mimo-smoke/deploy/
systemctl --user daemon-reload
# 先只报告不重建（默认）
bash ~/ru41-mimo-smoke/deploy/auto-update.sh --dry-run
# 看清差异后再开自动更新
touch ~/ru41-mimo-smoke/.auto-update.enabled
systemctl --user enable --now ru41-auto-update.timer
```

- **默认不重建**：没有 `.auto-update.enabled` 就只拉镜像、报告差异。
- 国内拉镜像走 NJU 镜像站（`ghcr.nju.edu.cn/windy664/...`），拉不动自动回退 ghcr.io 直连。
- sidecar 是 one-shot 容器，没有「运行中实例」可比对 → 直接给新镜像打 `zhihuishu-sidecar:local` 标签，下次挂机/采集自动用新版。
- ⚠️ 自动更新上线后，「人工看 CI 绿灯」这个闸就没了：建议让 `latest` 标签只在 `check` 全绿时更新
  （见 `docs/zhihuishu-watch.md` §2.4）。
