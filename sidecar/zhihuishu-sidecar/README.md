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
- 红线：只读采集，绝不进入答题页；空轮询不覆盖既有报告（防把已知 deadline 冲掉）
- 实测：8 课程 6 任务约 195s；新任务首见会提醒，之后每日最多提醒一次（Ru41 端控制）

## 首跑验证清单（对应 recon 文档 §6）

- [ ] 账密登录 + 滑块被 opencv 自动通过
- [ ] 课程被识别（防「新版 AI 课程」未适配）
- [ ] Xvfb 有头模式下进度正常累计
- [ ] `data/` cookies 跨天有效
- [ ] limitMaxTime=30min/课、每晚一次的节奏不触发人机验证
