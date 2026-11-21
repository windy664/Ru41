---
type: research-decision
status: proposed
target: 智慧树自动刷课（Autovisor sidecar）接入与「人不在家也能更新」链路
updated: 2026-11-21
tags: [Ru41, zhihuishu, autovisor, 刷课, sidecar, github-actions, systemd]
---

# 智慧树自动刷课：接入方案与更新链路

> 一句话：**刷课本体早就有（上游 Autovisor + 我们的 sidecar 封装），但它没接上定时器、
> 镜像也不在 CI 里，所以「自动」这一环是断的。**
> 本文给出三处补齐 + 一条「人不在家也能更新」的链路，并列出**必须回家验证**的清单。

## 1. 现状盘点（2026-11-21）

### 1.1 三个东西，别混

| 层 | 是什么 | 在哪 | 归属 |
|---|---|---|---|
| ① **刷课本体** | 视频挂机：自动登录、播、切集、跳弹题、倍速、刷习惯分 | 独立 sidecar 镜像 `zhihuishu-sidecar:local`（Playwright 官方镜像 + Xvfb 有头模式） | **上游 Autovisor**（CXRunfree/Autovisor，MIT，钉死 `5c71562`），我们只做封装 |
| ② 作业采集 | 枚举课程与任务、写 `report.json` | 同一个 sidecar，`scripts/collector.py` | **我们自研**（上游无现成方案） |
| ③ 课堂签到值守 | 抓老师发起签到时的真实接口 | 同一个 sidecar，`scripts/signin_watch.py` | **我们自研**（MITM 抓包） |

Ru41 主仓里的 `plugins/zhihuishu` 是**纯消费端**：只读 `report.json`，做 deadline 分类与提醒，
**不碰视频、不碰账号密码**。文档原话：「视频学习自动化同样不在 Ru41 内」。

### 1.2 三个缺口

| # | 缺口 | 证据 | 后果 |
|---|---|---|---|
| **G1** | **刷课没有定时器** | `sidecar/zhihuishu-sidecar/deploy/` 只有 `zhihuishu-collector.{service,timer}` 与 `zhihuishu-signin-watch.{service,timer}`，**没有跑 Autovisor 的单元** | 刷课只能手动 `docker compose run --rm zhihuishu` 跑一次 |
| **G2** | **sidecar 镜像不在 CI 里** | 主仓 `.github/workflows/ci.yml` 的 `paths-ignore` 排除了 `sidecar/**`；`image` job 只构建主镜像 | 镜像只能在生产机上手工 `docker compose build`（build context 还要先放上游 Autovisor 源码，`Autovisor/` 被 `.gitignore` 排除）→ 不可复现、也没法在 CI 里验 |
| **G3** | **人在外面时更新链断在最后一环** | CI 推 GHCR ✅ → 生产机在家 LAN、无公网入口 → 只能人 ssh 过去跑 `~/ru41-pull-image.sh` + `~/ru41-roll.sh` | 人在外面时改代码出不了镜像 |

## 2. 设计

### 2.1 G1：给刷课接上定时器

新增 `sidecar/zhihuishu-sidecar/deploy/zhihuishu-watch.{service,timer}`：

- **service**：`docker run --rm` 跑镜像自带 CMD（`Xvfb :99 + python Autovisor.py`），
  只挂 `config.ini`/`data`/`logs`，**不覆盖命令**；跑完即退（`RemainAfterExit=no`）。
- **timer**：每晚 **19:00**（`RandomizedDelaySec=600`、`Persistent=true`）。

**为什么是 19:00**：三个单元共用同一个 `data/` 目录（cookies 就在里面），必须错开：

| 时刻 | 单元 | 干什么 |
|---|---|---|
| 08–17 每 30 分钟 | `zhihuishu-signin-watch` | 白天巡课，监听签到接口 |
| **19:00** | **`zhihuishu-watch`（新）** | **刷课**（开浏览器，吃内存，可能 1–3 小时） |
| 23:30 + 04:30 | `zhihuishu-collector` | 作业采集，独占 cookies 续期，写 `report.json` |

刷课放在「真实使用时段」也是风控缓解的一条（深夜长时间挂机容易弹人机验证）。

**超时口径**：每门课时长由 `config.ini` 的 `limitMaxTime` 兜底（默认 30 min/课），
但总时长按课程数线性涨 → systemd 侧放宽到 3 小时（`TimeoutStartSec=10800`）。
被杀 = 进度只涨一半。

### 2.2 G2：sidecar 镜像进 CI

新增 `.github/workflows/sidecar-image.yml`（**单独一条**，不去动 `ci.yml` 的 `paths-ignore`）：

```
checkout 仓库
  → git clone 上游 Autovisor + checkout --detach 5c71562   # 钉死版本，可复现
  → git apply patches/*.patch                              # 我们的 Linux 安装器修复
  → docker build（sidecar/zhihuishu-sidecar/ 作 build context）
  → 推 ghcr.io/windy664/zhihuishu-sidecar:{latest,sha}
```

**要点**：
- 上游源码**不入库**（`.gitignore` 排除 `Autovisor/`），CI 里现拉现钉版本；
- 补丁在 CI 里 `git apply` —— 补丁挂了就构建失败（不会推出一个没打补丁的镜像）；
- `cache-from/cache-to: type=gha` 缓存层（镜像里有 playwright 浏览器，约 150MB）；
- 单独 workflow → `sidecar/**` 的改动**不再被 `ci.yml` 的 `paths-ignore` 吞掉**。

### 2.3 G3：生产机自更新（人不在家也能更新）

完整链路：

```
laptop(在外面)  ──push──▶  GitHub
                              │
                     CI: check(绿) ──▶ image: 推 GHCR
                              │
        ┌─────────────────────┴──────────────────────┐
   git-sync(可选)                              生产机自更新 timer(每 30 分钟)
   pending 分支 → 家里机器合并                    拉镜像(NJU 镜像站) → 比对 → roll.sh → 健康检查 → 失败回滚
```

**新增 `deploy/auto-update.{sh,service,timer}`**：

- 脚本 `auto-update.sh`：**拉镜像 → 比对运行中容器的 image id → 不同才 roll → 睡眠等容器起来
  → 逐个查「在跑 + RestartCount 没暴涨」→ 不健康就用旧镜像 id 回滚**。
- **安全闸**：
  1. 没有 `~/ru41-mimo-smoke/.auto-update.enabled` 这个文件 → **只报告不重建**。
     先在机器上手动跑 `--dry-run` 看清楚差异，确认后再 `touch` 开自动更新。
  2. 重建不健康 → 自动回滚（`roll.sh` 不删旧镜像，回滚随时可用）。
  3. sidecar 是 one-shot 容器，没有「运行中实例」可比对 → 直接把新镜像打上
     `zhihuishu-sidecar:local` 标签，**下次挂机/采集自动用新版**，无需重建。
- timer：每 30 分钟一轮（`OnBootSec=10min` + `Persistent=true`，开机补跑）。

**为什么不让 GitHub Actions 直接 ssh 生产机**：生产机在家 LAN、无公网入口，
Actions 的 runner 连不上它。**让机器自己拉自己滚**才是「人在外面也能更新」的正确形状
（`git-sync` 那个仓库说的也是同一件事：远端是会合点，LAN 机器自己出去取）。

> ⚠️ 前提：镜像要能被生产机拉到。国内走 NJU 镜像站（`ghcr.nju.edu.cn/windy664/...`），
> 直连 ghcr.io 会卡住。**sidecar 镜像能否被 NJU 镜像站代理，未验证** —— 这是 G3 的
> 唯一未决项，回来第一件事就是 `docker pull ghcr.nju.edu.cn/windy664/zhihuishu-sidecar:latest` 试一下；
> 拉不动就让脚本回退到 ghcr.io 直连（脚本已内置回退，但国内可能很慢）。

### 2.4 顺带解决：CI 的 `latest` 标签语义

`ci.yml` 现在让 `image` 与 `check` **并行**，注释里写明「check 红了镜像照推——但镜像推了也不会自动部署」。
一旦 G3 的自动更新上线，**「人工把关」这个闸就没了**：check 红也推 `latest` → 机器自动拉 → 坏镜像上生产。

**两个改法**（二选一，建议 A）：
- **A（推荐）**：新增 `publish-latest` job，`needs: check`，用
  `docker buildx imagetools create` 把 `:sha` 重新打成 `:latest`（不重新构建，很快）。
  `check` 红 → `latest` 不动 → 机器拉到的永远是绿过的镜像。
- **B**：`image` job 改 `needs: check`（串行，总耗时 ~7.5 分钟）。

## 3. 刷课「接进 Ru41」还差什么

现在 Ru41 只从 sidecar 拿到**作业**（deadline）和**内容解锁信号**（`content_published`，
即「这门的视频已经放出来了，可以安排自动学习」）。

**刷课进度没有回传**。想做「X 门课还差 N 分钟」需要：

1. sidecar 侧：从平台「学习进度」页读出每门课的已刷/总时长
   （Autovisor 自己会写 `data/study_*.json`，但那是调试用的页面 dump，不是结构化进度；
   需要新写一个采集脚本，**必须先在真机上抓一次页面结构**，盲写不了）；
2. 写进 `report.json`（契约加字段，如 `watch_progress: [{course, done_min, total_min}]`）；
3. Ru41 侧 `plugins/zhihuishu` 读它并播报（未解锁 → 可刷；解锁后 → 还差多少）。

**这一步是「开发不测试」做不了的**：页面结构得在有登录态的机器上抓。
所以本次只交付 2.1–2.3（能盲写的部分），进度回传列为下一步。

## 4. 必须回家验证的清单

### 4.1 刷课本体（recon §6 的 5 项，至今一条没勾）

- [ ] **水课是否属于「新版 AI 课程」**（带 AI 角标）——Autovisor 未适配这类，**决定刷课方案是否可用**
- [ ] 账密登录 + 滑块能否被 opencv 自动过
- [ ] Xvfb 有头模式下**进度是否正常累计**（不是「跑完但进度不涨」）
- [ ] `data/` cookies 跨天有效（决定重登频率）
- [ ] `limitMaxTime=30min/课` + 每晚一次的节奏会不会触发人机验证

### 4.2 本次新增的东西

- [ ] `docker pull ghcr.nju.edu.cn/windy664/zhihuishu-sidecar:latest` 能不能拉到（**G3 的唯一未决项**）
- [ ] `bash deploy/auto-update.sh --dry-run` 的输出符合预期，再 `touch .auto-update.enabled` 开自动更新
- [ ] 装 `zhihuishu-watch.timer` 后手动 `systemctl --user start zhihuishu-watch.service`，
      看 `journalctl --user -u zhihuishu-watch -f` 里 Autovisor 的首跑日志
- [ ] 首次别真开自动更新：先 `--dry-run` 看差异，再手动 `--apply` 试一次（确认回滚可用）

## 5. 风险与红线

| 风险 | 缓解 |
|---|---|
| 上游 Autovisor 2026-09 公告读研、「佛系更新」→ 页面改版就失修 | 钉死 sha 不跟最新版；备胎 `fuckZHS`（2446★，2025-10 已停更）；作业线不受影响 |
| 账号被封 = 整条省时流水线归零 | **降频保号优先**：limitMaxTime 30min/课、每晚一次、只在真实使用时段跑 |
| 自动更新把坏镜像推上生产 | `latest` 只在全绿时更新（§2.4 A）；机器侧健康检查不过自动回滚；默认不启用（enable 文件闸） |
| 三单元共用 cookies 目录互相踩 | 三个单元时间错开（§2.1） |
| **考试永不自动化**；自动提交作业不做 | 考试式作业（30 分钟倒计时 + 摄像头 + 切屏记录）一律人工；采集器只读规则元数据，判为考试式即止步 |

## 6. 交付物

| 文件 | 作用 |
|---|---|
| `.github/workflows/sidecar-image.yml` | sidecar 镜像进 CI（上游钉版本 + 套补丁 + 推 GHCR） |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-watch.service` | 刷课单轮单元（跑完即退） |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-watch.timer` | 每晚 19:00 拉起刷课 |
| `deploy/auto-update.sh` | 生产机自更新：拉镜像 → 比对 → 滚动 → 健康检查 → 回滚 |
| `deploy/auto-update.service` / `.timer` | 每 30 分钟自更新一轮 |
