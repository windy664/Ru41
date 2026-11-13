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
# 3. 验证通过后，systemd timer 每晚定时拉起同款命令（跑完即退）
```

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
