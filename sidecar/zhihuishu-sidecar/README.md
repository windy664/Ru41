# zhihuishu-sidecar

智慧树水课视频挂机 sidecar：Autovisor 跑在官方 Playwright 镜像里，Xvfb 有头模式。

- 调研与决策：`../../docs/zhihuishu-recon.md`
- 部署约定：Ru41 `../../deploy/README.md` §5 —— 重依赖（浏览器/Python）独立 sidecar 镜像，与 Ru41 零网络耦合
- 上游：https://github.com/CXRunfree/Autovisor （pinned `5c71562`，2026-10-18）
- 浏览器：镜像内 Chromium，经 `/usr/local/bin/zhihuishu-chromium` symlink 提供给 `EXE_PATH`

## 使用

```bash
# 0. 填 config.ini：username / password / URL1…（zhihuishu.com 网页版进课程后复制地址栏）
# 1. 构建
docker compose build
# 2. 手动跑一次，看日志验证
docker compose run --rm zhihuishu
# 3. 装定时器（每晚 19:00 刷课，跑完即退）
cp deploy/zhihuishu-watch.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now zhihuishu-watch.timer
systemctl --user start zhihuishu-watch.service   # 立即试跑一轮
journalctl --user -u zhihuishu-watch -f
```

**镜像由 CI 出**（`.github/workflows/sidecar-image.yml`）：上游 Autovisor 按钉死的 sha 拉取、
套 `patches/` 再构建，推 `ghcr.io/windy664/zhihuishu-sidecar`。生产机只需 `docker pull`。
本地构建需要先把上游源码放进 build context：`git clone <上游> Autovisor && git -C Autovisor checkout --detach 5c71562`。

**三个单元共用 `data/`（cookies 在里面），必须错开**：
`zhihuishu-signin-watch`（白天 08–17 每 30 分钟）→ **`zhihuishu-watch`（每晚 19:00 刷课）**
→ `zhihuishu-collector`（23:30 + 04:30 采集作业）。

## 刷课目标是自动算出来的（2026-11-21）

**Autovisor 不会自己判断刷哪些课** —— 它只按 `config.ini` 的 `[course-url]` 段逐个跑。
这份清单现在由 `scripts/plan_targets.py` 每晚 18:45 自动生成：

```
23:30 采集器 → report.json（含每门课的「总学习进度」百分比）
18:45 目标生成器 → 选出「有内容可刷 + 没刷完」的课 → 写 config.ini 的 [course-url] 段
19:00 Autovisor → 按清单刷（limitMaxTime 30min/门）
```

```bash
python3 scripts/plan_targets.py --dry-run   # 只报告不改文件
python3 scripts/plan_targets.py             # 真写 config.ini + data/targets.json
python3 -u scripts/test_plan_targets.py     # 自测
```

- 人工在 `config.ini` 里手写的 URL **保留并排在自动选出的前面**（手放进去的必有理由）。
- 只改 `[course-url]` 段的选项行，**注释与其余段落原样保留**（防风控的关键注释不能丢）。
- 选课规则：`content_published is True`（有内容）且 `progress` 读不到或 `< 100`（没刷完）；
  每晚最多 3 门（`--max`），避免一晚刷不完反而被风控盯上。

## 作业采集器（collector）

`scripts/collector.py` 产出 `data/report.json`（契约见 Ru41 `docs/sources-zhihuishu.md`），
由 Ru41 `modules/zhihuishu` 消费做作业 deadline 提醒。与视频挂机相互独立。

```bash
docker run --rm \
  -v "$PWD/config.ini:/app/config.ini" \
  -v "$PWD/data:/app/data" \
  -v "$PWD/scripts:/scripts:ro" \
  zhihuishu-sidecar:local sh -c '
    Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5
    DISPLAY=:99 python -u /scripts/collector.py
    kill $XPID 2>/dev/null'
```

- 登录：cookies 有效直接用；失效自动账密重登（opencv 滑块），成功后刷新 `data/cookies.json`
- 课程：门户 `.course-card` 枚举全量课程；识别失败可 `--courses "课名1,课名2"` 兜底
- 任务：点击卡片进入课程页截获 `activityTask/list` 响应，按 `activityTaskId` 合并去重
- 完成态三态化：`True`=已完成 / `False`=未完成 / `null`=平台没给（未知）。未知**不能**当成未完成——
  消费端会把「已完成→未完成」当成「老师打回」重建待办并反复提醒（2026-11-13 实况）
- wenda（老平台）完成态字段：`answerScore`/`isShowScore` 有值 → 已批阅；`status` 1=已提交、4=已批阅、
  0=未提交。`isComplete` 字段不可信（已批阅的作业也报 0），不采信
- 全局待办（`findNoFinishTaskTotal`）只用于补「逐课流没有的任务」与缺失的截止时间，
  **不覆盖**逐课流的完成态与 ddl——全局流给的是课程级日期（2026-11-13 实况：
  职业生涯规划 3 个课前测的 ddl 被从 10-31 改写成 11-30）
- 红线：只读采集，绝不进入答题页；空轮询不覆盖既有报告（防把已知 deadline 冲掉）
- 实测：8 课程 6 任务约 195s；新任务首见会提醒，之后每日最多提醒一次（Ru41 端控制）

### 容器内回归测试

```bash
docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
  python -u /scripts/test_completed_states.py      # 完成态三态化
docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
  python -u /scripts/test_merge_no_false_drift.py   # 全局待办合并不改写逐课真值
docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
  python -u /scripts/test_normalize_time.py         # 时间归一化
```

## 首跑验证清单（对应 recon 文档 §6）

- [ ] 账密登录 + 滑块被 opencv 自动通过
- [ ] 课程被识别（防「新版 AI 课程」未适配）
- [ ] Xvfb 有头模式下进度正常累计
- [ ] `data/` cookies 跨天有效
- [ ] limitMaxTime=30min/课、每晚一次的节奏不触发人机验证
