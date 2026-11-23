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
23:30 采集器 → report.json（含每门课的「总学习进度」百分比 + 上游目录类型 catalog）
18:45 目标生成器 → 选出「有内容可刷 + 没刷完 + 有刷课器认得」的课 → 分流
19:00 挂机      → 按分流结果刷（limitMaxTime 30min/门）
```

```bash
python3 scripts/plan_targets.py --dry-run   # 只报告不改文件
python3 scripts/plan_targets.py             # 真写 config.ini + data/targets.json
python3 -u scripts/test_plan_targets.py     # 自测
```

- 人工在 `config.ini` 里手写的 URL **保留并排在自动选出的前面**（手放进去的必有理由）。
- 只改 `[course-url]` 段的选项行，**注释与其余段落原样保留**（防风控的关键注释不能丢）。
- 选课规则：`content_published is True`（有内容）且 `progress` 读不到或 `< 100`（没刷完）
  且 **有刷课器认得这门课的页面**（见下）。
- 每晚最多 3 门（`--max`），避免一晚刷不完反而被风控盯上。

## 两个刷课引擎（2026-11-21 首跑后加的）

上游 Autovisor 认课**不靠域名、靠 DOM 选择器**（`modules/lesson_navigation.py` 的
wisdom/fusion/hike/legacy 四类），四类都不命中就抛「课程目录加载超时」→ 该门课失败 →
**整轮 break**（`Autovisor.py`：`if outcome is CourseOutcome.FAILED: break`）。

生产机实测：门户上 9 门课分属 `ai-smart-course-student-pro.zhihuishu.com` /
`hike-teaching-center.polymas.com` / `wenda.zhihuishu.com` 三个平台，
**四类选择器全部命中 0** → 上游一门都刷不了。所以分成两个引擎：

| 引擎 | 谁的课 | 跑什么 | 清单在哪 |
|---|---|---|---|
| `autovisor` | 上游认得的课（目前 0 门） | 上游 Autovisor | `config.ini` 的 `[course-url]` |
| `polymas` | 南京大学 AI教学中心的课（5 门） | **自研** `scripts/brush_polymas.py` | `data/targets.json`（带 `engine` 字段） |

自研脚本的流程（每步都实测过）：课程主页 → 点「我的学习」→ 点「学习资源」→
列出资源 → 跳过已完成（`.agent-course-form-item-checkbox success`）与非视频 →
点未完成的 mp4 → 资源详情页 → iframe 里找到 `<video>` → 播到结束 → 回课程页 → 下一个。

实测：看完一个 4 分 07 秒的 mp4，「总学习进度」从 0% 变 20%（5 个资源完成 1 个）→ 平台认「看过了」。

```bash
# 手动试跑一轮(容器内)
docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
  -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
  sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
        DISPLAY=:99 python -u /scripts/brush_courses.py --dry-run; kill $XPID 2>/dev/null'

# 每晚定时挂机(自研脚本, 19:00)
cp deploy/zhihuishu-brush.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now zhihuishu-brush.timer
```

风控纪律（降频保号优先）：每门课 30 分钟（`config.ini` 的 `limitMaxTime`）、每晚最多 3 门、
倍速封顶 1.5、只在 19:00 跑、已完成的自动跳过。

### ai-smart 的三种页面形态（2026-11-24 摸清）

同一个 `ai-smart-course-student-pro.zhihuishu.com` 域下其实有**三种**课，页面结构完全不同，
认错了就会「看着没内容其实有」：

| 形态 | 怎么认 | 资源在哪 | 怎么刷 |
|---|---|---|---|
| 视频课 | 主页有「课程视频」入口 | 「课程视频」页的章节列表（带「视频」标签的行） | `brush_aismart.py`：逐个点开，同页 video.js 播放器 |
| **图谱课**（新形态） | 主页**没有**「课程视频」 | 挂在**知识点**下：点知识点卡片 → `/learnPage/<course>/<knowledgeId>/<chapter>` → 右侧「切换学习资源」 | `brush_aismart_graph.py`：逐个知识点 → 逐个资源 → 点开 |
| 内容未发布 | 「课件资料」写着「暂无学习资源」 | — | 跳过 |

图谱课（实例：「高等数学I(理)」，56 个知识点）几个要点：

- 「课件资料」tab 显示「共 0 个学习资源」，**但课不是空的** —— 别拿它判断有没有内容。
- 左侧目录每条知识点带「必学 n/m」（`.section-item-collapse-info` → `.progress-text`），
  这才是图谱课的进度口径（不是知识学习页那个「学习进度 X%」）。
- 右侧资源卡片是 `.basic-info-video-card-container`，文本形如「二、函数 已完成」「38页 01-02 函数.pptx」。
- **点开资源 → 前端自己发 `stu/studyRecord/completed` → 资源变「已完成」→ 「必学」计数上涨**。
  实测「函数的概念」必学 0/2 → 2/2。
- ⚠️ 这些上报的请求体是**加密**的（`secretStr`），所以只能走真实浏览路径，伪造不了 ——
  这也正好符合「不替平台造数据」的纪律。

采集器第三步会探 learnPage 的知识点目录，所以图谱课不会再被当成「没内容」踢出自动队列。

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
