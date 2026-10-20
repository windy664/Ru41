---
type: research-decision
status: decided
target: 知到/智慧树（水课视频 + 作业）
updated: 2026-10-18
tags: [Ru41, zhihuishu, 水课, 作业, docker]
---

# 智慧树水课方案调研与决策

**目标：** 把水课视频与作业的时间成本压到最低，省下的整块时间投入科研/项目/比赛。
**核心原则：** 别人弄好的东西不自研。视频层零自研，Ru41 只做消费端。
**决策状态：** 已定（Autovisor + Docker），待部署验证。

## 1. 生态调研结论（2026-10）

| 类别 | 代表项目 | 状态 | 结论 |
|---|---|---|---|
| 浏览器自动化 | ocsjs（3741★，活跃）、Autovisor、fuckZHS | ✅ 活跃 | 视频层直接用 |
| 纯 API 逆向 | zhihuishu-tool（2023 归档）、zhihuishu-cli（失修） | ❌ 全死 | 不可依赖；接口已变 |
| 作业 API 逆向 | 无 | ❌ 空白 | 作业线是 Ru41 的自研价值区 |

- 活跃项目全是浏览器自动化：页面自己的 JS 负责签名，绕开接口加密的猫鼠游戏。
- OCS 桌面版有检测绕过，但 Windows 优先（Ubuntu 未测试），不依赖。
- 作业读取无任何现成方案；作业自动提交风险高，本项目永久不碰考试。

## 2. 选型决策

**主选：[Autovisor](https://github.com/CXRunfree/Autovisor)**（1002★，MIT）
- Python + Playwright，明确按"无人监督"设计，有免安装版
- 2026-09 仍在发版（3.18.4）；反检测逻辑持续更新是刷课脚本的命脉
- 支持自动登录、自动过滑块（opencv）、自动播/切集/跳弹题、倍速、静音、刷习惯分
- 覆盖智慧共享课、翻转课、融合共享课

**备胎：[fuckZHS](https://github.com/VermiIIi0n/fuckZHS)**（2446★，MIT，2025-10 停更）。
**淘汰：** 自拼 Playwright+Tampermonkey+ocsjs（本质是造轮子）、OCS 桌面版（Windows only）。

**自研边界：**
- 视频挂机/弹题：Autovisor，零自研
- 进度/作业截止进 Ru41 简报：暂缓，用两周真觉得烦再写胶水（唯一候选自研点）
- 作业草稿 + 审核：Ru41 空白区价值，视频层跑稳后再启动（走 analysis_v1 管线 + 审批）
- 自动提交作业：不做（等审核链路成熟，P4）；考试：永不

## 3. Autovisor 关键事实（源码/README 调研）

- 依赖全跨平台：`playwright>=1.49,<2`、httpx、pillow、requests；`pygetwindow` 仅 Windows 条件依赖，Linux 安装不受影响；Python `>=3.10,<3.14`
- 配置极简（config.ini）：账号密码、浏览器（driver=Edge/Chrome + EXE_PATH）、enableAutoCaptcha、`limitMaxTime`（每课时长上限，min）、`limitSpeed`（≤1.8x）、多个课程 URL
- 登录 Cookies 存 `data/` → 卷挂载即可持久会话
- 入口 `Autovisor.py`；GUI（GUI.py）可弃；uv 管理依赖
- 入口无 headless 开关（默认拉起系统浏览器）；`enableHideWindow` 走 Win32 API，Linux 不可用也不需要

**已知限制（README）：**
- 长时间挂机可能弹人机验证，程序暂停等人工 → **不 24h 挂机，每晚定时 + limitMaxTime 兜底**
- 新版「AI 课程」（带 AI 角标）未适配 → 首跑验证目标
- 最小化窗口进度不涨 → 容器内用 Xvfb 有头模式规避
- 非 PPT/PDF 章节要手动切；弹题检测非实时
- 2026-09 公告：作者读研，「佛系更新」→ 维护降速是最大中期风险，备胎要留

## 4. 部署方案（B：独立 sidecar 容器）

遵循 `deploy/README.md` 第 5 节扩展约定：重依赖（浏览器 / Python 运行时）走独立 sidecar 镜像，不进 Ru41 主镜像，不挂 Docker socket。本 sidecar 与 Ru41 零网络耦合，只靠 `report.json` 文件桥。

**落位：** 独立目录（如 `~/Project/zhihuishu-sidecar/`），不进 Ru41 仓库——避免把第三方仓库拖进 Ru41 的 build context；Ru41 侧未来只加 `modules/zhihuishu` 读取端。

```dockerfile
FROM mcr.microsoft.com/playwright/python:v1.49.0-jammy   # 镜像自带 Chromium 与全部系统依赖
RUN apt-get update && apt-get install -y xvfb && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Autovisor/ .
RUN pip install --no-cache-dir .
CMD ["xvfb-run", "--auto-servernum", "python", "Autovisor.py"]
```

```yaml
# docker-compose.yml（草案）
services:
  zhihuishu:
    build: .
    init: true
    volumes:
      - ./config.ini:/app/config.ini:ro     # 配置（含账号，勿入 git）
      - ./data:/app/data                    # 登录 Cookies 持久化
      - ./logs:/app/logs
      - /home/windy/Project/Ru41/.ru41/zhihuishu:/report  # Phase 2：Ru41 读取端
```

要点：
- **EXE_PATH 指向镜像内 Chromium**（`/ms-playwright/chromium-*/chrome-linux/chrome`），绕开"默认找系统 Edge"
- **Xvfb 有头而非 headless**：规避 headless 检测与"最小化不涨进度"两类问题；若实测新版 headless 也能过，可去掉 Xvfb 一层
- **触发**：宿主机 systemd timer 每晚固定时段 `docker compose run --rm zhihuishu`，跑完即退（one-shot 用法，同 deploy 文档 `codex login` 的模式）；不在容器里常驻 cron
- **首跑**：VNC 或 `docker exec` 截图观察登录滑块与课程识别；enableAutoCaptcha=True 让 opencv 过滑块
- **备份**：`data/`（cookies）与 report 卷纳入 deploy 文档的异地备份纪律；容器本身无状态，断电后 timer 自动拉起
- 成本：磁盘 ~1.5GB（镜像），内存峰值 ~1.5GB，每晚 ≤1-2 小时（limitMaxTime 兜底）

## 5. 风险与回退

| 风险 | 缓解/回退 |
|---|---|
| 水课属于新版「AI 课程」，Autovisor 不识别 | 换 fuckZHS 试 → 再不行 OCS（找 Windows 机器） |
| 智慧树检测自动化、弹人机验证频繁 | 只在真实使用时段跑 + 降倍速 ≤1.5x + limitMaxTime 缩短 |
| 作者佛系更新，页面改版失修 | fuckZHS 备胎；极端情况视频回手动，作业线不受影响 |
| 账密登录触发滑块/扫码 | enableAutoCaptcha；仍不行则首跑人工登录后靠 data/ cookies 续期 |
| 服务器断电 | 无常驻状态，timer 自动拉起；cookies 在卷里 |

**工程纪律：一切写回平台的动作（提交作业）必须过审核链路；考试永不自动化。账号被封 = 整条省时流水线归零，降频保号优先。**

**作业分类红线（2026-10-19 实测确认）：** 智慧树「作业」分两类——
1. **考试式作业**（如形势与政策作业）：`hiexam.zhihuishu.com` 承载，30 分钟倒计时 + 摄像头 + 切屏记录，点开即计时且不可暂停。**一律人工作答**，采集器只读 deadline 与规则元数据，禁止进入验证页之后的任何内容。
2. **普通作业**：无计时无监考 → Codex 草稿管线适用，人工审核后手动提交。
采集器读题目内容前必须先读规则元数据分类；分类为考试式即止步。

## 6. 开放问题（首跑验证清单）

- [ ] 水课是否属于「新版 AI 课程」类型（决定 Autovisor 可用性）
- [ ] 账密登录是否顺畅，滑块能否被 opencv 自动过
- [ ] Xvfb 有头模式下进度是否正常累计
- [ ] limitMaxTime=30min/课、每晚一次的节奏下是否触发人机验证
- [ ] 镜像内 Chromium 路径与 EXE_PATH 的实际写法
- [ ] data/ cookies 跨天有效性（决定重新登录频率）

## 7. 后续路线

1. Phase 1（本文档后）：部署容器，首跑验证第 6 节清单
2. Phase 2（可选）：进度/作业截止进 Ru41 每日简报——新 source adapter 读 Autovisor 输出或平台页面，纯附属
3. Phase 3：作业题目 → Codex 草稿（标不确定项）→ QQ/HTTP 审核闭环（走现有 analysis_v1 + Interaction 管线）
4. 自动提交：不做，除非审核链路成熟且有真实需求
