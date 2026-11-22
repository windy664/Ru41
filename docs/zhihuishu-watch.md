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

## 3. 刷课「接进 Ru41」：目标从人工清单变成自动推导（2026-11-21 已做）

**先答一个关键问题：Autovisor 会不会自己判断刷哪些课？——不会。**
`modules/configs.py: get_course_urls()` 只从 `config.ini` 的 `[course-url]` 段读一串
`URL1=… URL2=…`，`Autovisor.py:276` 就是 `for index, course_url in enumerate(config.course_urls, 1)`
——**它被告知刷哪些课**。也就是说「自动刷课」一直缺的不是刷的能力，是**目标的来源**。

**「哪些课还没刷完」的数据源本来就有**（2026-11-21 在上游源码里查实的）：

| 想要的信息 | 有没有 | 在哪 |
|---|---|---|
| 平台上有哪些课 | ✅ | 采集器枚举门户 `.course-card`（实测 8~9 张卡 → 11 门课） |
| 哪门课有内容可刷 | ✅ | `content_published`（点「我的学习」数「共 N 个学习资源」） |
| **这门课刷到百分之几** | ✅ **但没人取** | 同一次探测读到的文本里就有「总学习进度 42%」——采集器只解析了资源数，把进度丢掉了 |
| 当前课时的播放进度 | ⚠️ | Autovisor 有 `modules/progress.py: get_course_progress()`，但只 `\r` 打印到 stdout，**不落文件** |

所以补的是**我们自研的一环**，不动上游：

1. **采集器**（`scripts/collector.py`）：`detect_content_published` → `detect_course_state`，
   同一次点开里把「总学习进度 X%」一起解析出来 → `report.json` 的 `courses[].progress`。
   （判断「有没有内容」仍**只用资源数**——「总学习进度」恒在，用它判断会假阳性。）
2. **目标生成器**（`scripts/plan_targets.py`，纯 stdlib，宿主上跑）：
   读 `report.json` → 选出「`content_published is True` 且（`progress` 为 null 或 `< 阈值`）」的课
   → **写回 `config.ini` 的 `[course-url]` 段** → Autovisor 下次跑就按这份清单刷。
   - 只改那一段的选项行，**注释与其余段落原样保留**（`config.ini` 里有防风控的关键注释，
     用 configparser 重写会全丢）；
   - 人工维护的 URL 保留并排在自动选出的前面（他手放进去的必有理由），与自动选出去重；
   - 顺带写 `data/targets.json`：今晚刷哪些、各自还差多少（给 Ru41 侧播报用）。
3. **定时器**（`deploy/zhihuishu-targets.{service,timer}`）：每晚 **18:45** 生成目标，
   赶在 19:00 的刷课之前。

完整时间线：

```
23:30 采集器 → report.json（含 progress）      ┐
18:45 目标生成器 → config.ini [course-url]     ├─→ 19:00 Autovisor 按清单刷
19:00 刷课（limitMaxTime 30min/门）            ┘
```

**自测**：`scripts/test_plan_targets.py`（选课规则 / 只改 `[course-url]` 段 / 注释不丢 /
人工清单优先 / 幂等 / 端到端），已本地跑通。

**还没做的**：Ru41 侧消费 `targets.json` 播报「今晚刷 X 门，还差 Y 分钟」——
契约字段有了，但 Ru41 插件还没读它（下一步）。

## 4. 首跑结论：**上游 Autovisor 刷不了我们的课**（2026-11-21 夜，生产机实测）

recon §6 的第一条清单终于能回答了 —— 答案是**不行**，而且不是配置问题，是结构问题。

### 4.1 首跑日志（`docker run zhihuishu-sidecar:local`，限时 3 分钟，一门课）

```
[INFO] 正在启动chrome浏览器...
[INFO] 已加载 Cookies!  (27 条)
[INFO] 使用Cookies登录成功!
[INFO] ===== 课程 1/1 =====
[INFO] 正在加载播放页...
[ERROR] 程序运行时出现未处理异常.
异常类型: RuntimeError
异常详情: 课程目录加载超时，且未检测到可处理的安全验证
  File "/app/Autovisor.py", line 295 → detect_catalog_after_verification(page, page.url)
```

**好消息**：镜像内 Chromium（156）起得来、Xvfb 有头模式正常、**Cookie 登录一次成功**、
后台任务（人机验证监听 / 视频优化 / 答题跳过 / 播放）全部启动。

**坏消息**：课程目录认不出来 → `detect_catalog` 抛 RuntimeError。

### 4.2 为什么认不出来：上游认课靠 DOM，不靠域名

上游 `modules/lesson_navigation.py` 只有四类课程目录选择器：

| 目录 | 选择器 | 对应平台 |
|---|---|---|
| `wisdom` | `.child-info.hasvideo` | 新版 AI 课（智慧树 ai-smart） |
| `fusion` | `.chapter-item` | 融合课（fusioncourseh5） |
| `hike` | `.file-item` | hike.zhihuishu.com |
| `legacy` | `.clearfix.video` | 旧版 |

我们门户上的 9 门课分属**三个平台**，四类选择器在它们的代表页面上**全部命中 0**：

| 平台 | 门数 | 代表 URL | 四类选择器命中 |
|---|---|---|---|
| `ai-smart-course-student-pro.zhihuishu.com` | 2（程序设计基础、高等数学I(理)） | `/singleCourse/knowledgeStudy/…` | 0 0 0 0 |
| `hike-teaching-center.polymas.com` | 5 | `/agent-course-full/…/stu/course-home` | 0 0 0 0 |
| `wenda.zhihuishu.com` | 2 | `/stu/courseInfo/studyResource?courseId=…` | — |

**而且就算补上选择器也还不够**：上游的跑课循环要求「视频与目录同页」——
`page.wait_for_selector("video")`（`course_runner.py`），而 polymas 点资源会**跳页**、
播放器在 **iframe** 里（`polymas.com/resource/online/preview/?fileId=…`）。

**最要命的一条**：上游 `Autovisor.py` 里 `if outcome is CourseOutcome.FAILED: break`
——**一门课失败就 break 掉整轮**，后面的课一节都刷不到。所以把不认得的课放进
`config.ini` 的 `[course-url]` 不是「少刷几门」，是**一节都刷不到**。

### 4.3 于是有了「刷课引擎」这层分流

```
采集器 → report.json（含 progress + catalog）
   ↓
目标生成器 plan_targets.py → 按引擎分流
   ├── engine=autovisor（catalog 有值）→ 写 config.ini [course-url] → 上游 Autovisor
   └── engine=polymas（polymas 域名）  → 写 data/targets.json      → 自研 brush_polymas.py
```

`config.ini` 的 `[course-url]` 是**上游专用**清单（只装它认得的课）；
自研引擎那份在 `data/targets.json` 里，带 `engine` 字段。

### 4.4 自研挂机脚本 `scripts/brush_courses.py`（按平台分发）

流程（每一步都在生产机实测过）：

```
课程主页 → 点「我的学习」→ 点「学习资源」标签
→ 列出资源(.el-tree-node__content) → 跳过已完成(success 标记)的 / 非视频(doc,pdf)
→ 点未完成的 mp4 → 资源详情页 → iframe 里找到 <video> → 播放到结束
→ 回课程页 → 下一个
```

| 事实 | 值 | 怎么拿到的 |
|---|---|---|
| 资源条目 | `.el-tree-node__content` > `.node-left` > `.file-name` | DOM dump |
| **已完成标记** | `.agent-course-form-item-checkbox **success**`（未完成是 `info`） | 看完一个视频前后对比两行的 `node-actions` |
| ⚠️ 勾选框不是完成标记 | `el-checkbox is-checked` **全都**是勾上的 | 同上（差点踩坑） |
| 课程整体进度 | 「总学习进度 X%共 N 个学习资源」 | 页面文本 |
| 播放器 | iframe `polymas.com/resource/online/preview/?fileId=…` 里的 `<video>` | 截图 + frame 枚举 |
| **平台认不认「看过了」** | **认** | 看一个 4 分 07 秒的 mp4 → 总学习进度 **0% → 20%**（5 个资源完成 1 个） |

**风控纪律**（recon §5「降频保号优先」）：每门课时限 30 分钟（`config.ini` 的 `limitMaxTime`）、
每晚最多 3 门、倍速封顶 1.5（平台最高 1.8）、只在 19:00 跑、已完成的自动跳过
（否则每天重复刷同样的视频）。

**自测**：`scripts/test_polymas_state.py`（进度解析 / 完成标记 / 视频识别 / 待刷清单），
纯 stdlib，宿主与 CI 都能跑。

### 4.5 首跑清单现状

- [x] **水课是否属于「新版 AI 课程」** —— **不是**。水课在 `hike-teaching-center.polymas.com`
      （南京大学 AI教学中心），上游四类目录选择器全不命中；已改走自研脚本。
- [x] 账密登录 + 滑块 —— 本轮用的是 **Cookie 登录（27 条 cookie，一次成功）**，没走账密滑块。
      （智慧树自己的账密一直可用，登录本身没问题；具体凭证不写进仓库。）
- [x] **Xvfb 有头模式下进度正常累计** —— 看一个 4 分 07 秒的 mp4，总学习进度 0% → 20%。
- [ ] `data/` cookies 跨天有效 —— 待观察（采集器每晚 23:30 会自动账密重登续期）。
- [ ] `limitMaxTime=30min/课` + 每晚一次会不会触发人机验证 —— 待观察（首轮 3 门连刷没弹）。
- [x] 镜像内 Chromium 路径与 `EXE_PATH` —— `/usr/local/bin/zhihuishu-chromium` 可用。

### 4.6 ai-smart 新版 AI 课：能播，但平台端看不到进度（2026-11-22 实测）

补上了这条线(program design: 程序设计基础 / 高等数学I(理) 在 `ai-smart-course-student-pro.zhihuishu.com`)。

**结构**：课程主页(knowledgeStudy) → 点「课程视频」→ courseVideoShare 页, 章节列表里
带「视频」标签的行 = 可刷的视频(程序设计基础 18 个) → 点「视频」→ **同页**出现
video.js 的 `<video>`(跟 polymas 的 iframe 不同)。高等数学那门**根本没有资源**
(课件资料页写着「共 0 个学习资源」)→ 不算漏刷。

**三个坑**（都写进代码注释了）：
1. 一行可能有**多个**标签(`['1个知识点', '视频']`)—— 只看第一个标签会漏掉一大半(5/18);
2. 视频列表渲染慢：刚进页面只有 5 行，要等；**不能提前点**「课程视频」，否则只渲染一半;
3. 平台在视频条目上**不给「已学」标记** → 用本地状态文件(`data/aismart_watched.json`)
   记住我们已经播完哪些, 否则每天重刷同样的视频。

**平台到底记不记账（诚实的结论）**：
- 播完 1.1数据类型(580s)后，**再点一次从 1.2s 开始播**（不续播）；
- 知识学习页的知识点「学习进度」不变(2%)、课件资料页的「总学习进度」也不变
  (视频不算那边的「学习资源」)；
- 但播放时播放器**确实在往平台 POST**（`collector2c.zhihuishu.com/public/kafkaCollect`
  埋点 + `kg-ai-run.zhihuishu.com/run/gateway/...` 一堆课程接口）。

所以：**学生端看不到视频进度口径，老师后台大概率看观看时长，但学生端验证不了**。
对比 polymas：那边「总学习进度 X%共 N 个学习资源」是**真的会涨**的(实测 0%→60%)。
这条线的跳过完全依赖本地状态文件（播完 18 个就不再刷了, 不会无限重复）。

## 5. 必须回家验证的清单

### 5.1 刷课本体（recon §6 的 5 项）

见 §4.5（4 项已验证，2 项待观察）。

### 5.2 本次新增的东西

- [x] `docker pull ghcr.nju.edu.cn/windy664/zhihuishu-sidecar:latest` 能不能拉到 —— **能代理**，
      但拉 3GB 会中途断流（`unexpected EOF`）→ `auto-update.sh` 已加重试（已下完的层留在本地）
- [ ] `bash deploy/auto-update.sh --dry-run` 的输出符合预期，再 `touch .auto-update.enabled` 开自动更新
- [x] 装 `zhihuishu-brush.timer` 后手动 `systemctl --user start zhihuishu-brush.service`
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
| `.github/workflows/sidecar-image.yml` | sidecar 镜像进 CI（上游钉版本 + 套补丁 + 推 GHCR + 跑自测）✅ 已跑通 |
| `.github/workflows/publish-latest.yml` | `:latest` 只在 CI 全绿时更新（给自动更新上保险）✅ 已跑通 |
| `sidecar/zhihuishu-sidecar/scripts/brush_courses.py` | **刷课入口**：按课程 URL 分发到对应平台（真正能刷的那个）✅ 实测刷完一个视频、进度 0%→20% |
| `sidecar/zhihuishu-sidecar/scripts/brush_polymas.py` | polymas 平台实现（我的学习 → 学习资源 → 逐个 mp4，播放器在 iframe） |
| `sidecar/zhihuishu-sidecar/scripts/brush_aismart.py` | ai-smart 新版 AI 课实现（课程视频 → 章节 → 逐个视频，播放器在同页 video.js） |
| `sidecar/zhihuishu-sidecar/scripts/polymas_state.py` | polymas 页面解析的纯函数（进度 / 完成标记 / 视频识别） |
| `sidecar/zhihuishu-sidecar/scripts/test_polymas_state.py` | 上面两个的自测（CI 跑） |
| `sidecar/zhihuishu-sidecar/scripts/course_state.py` | 采集器的纯函数（进度解析 + 报告条目字段） |
| `sidecar/zhihuishu-sidecar/scripts/test_report_entry.py` | 报告条目字段的回归测试（防「字段悄悄丢了」） |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-brush.service` / `.timer` | 每晚 19:00 刷水课（自研脚本，两个平台） |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-watch.service` / `.timer` | 每晚 19:00 跑上游 Autovisor —— **目前先不 enable**（没有它认得的课） |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-targets.service` / `.timer` | 每晚 18:45 生成刷课目标（分流到两个引擎） |
| `sidecar/zhihuishu-sidecar/scripts/plan_targets.py` | 目标生成器：报告 → 「今晚刷哪些课、谁刷」 |
| `sidecar/zhihuishu-sidecar/scripts/brush_ahk.py` | 学习平台(在线好课 gzus.aihaoke.net)实现：借 CAS TGT 登录 → 任务列表点「To learn」→ 真播 |
| `sidecar/zhihuishu-sidecar/scripts/no_video_courses.py` | 「这门课没有视频资源」的记忆(带 14 天时效)—— 别让空课占满目标清单名额 |
| `sidecar/zhihuishu-sidecar/scripts/test_no_video_courses.py` | 上面两个的回归测试(纯 stdlib, CI 跑) |
| `sidecar/zhihuishu-sidecar/deploy/zhihuishu-ahk-brush.service` / `.timer` | 每晚 21:00 刷学习平台(在线好课)的视频课 |
| `sidecar/zhihuishu-sidecar/scripts/test_plan_targets.py` | 目标生成器自测（CI 跑） |
| `deploy/auto-update.sh` / `.service` / `.timer` | 生产机自更新：拉镜像（带重试）→ 比对 → 滚动 → 健康检查 → 回滚 |

## 7. 学习平台「在线好课」(gzus.aihaoke.net) 的刷课

用户给的课程页: `https://gzus.aihaoke.net/student/course/29668/task?instanceId=11187&taskType=all-tasks&columnId=708120`
(广州软件学院 - 在线好课, 知途好课 aihaoke 平台)。

**登录(不用密码/验证码)**: 借 jwxt 插件已有的 CAS TGT ——
把它当 `CASTGC` cookie 注入(`.gzus.edu.cn`), CAS 认出 TGT 直接放行 →
passport 建会话 → 拿到 `haoke-token`。

```
课程页 → passport.aihaoke.net/auth/login(学校「广州软件学院」已自动带出)
  → 302 gzus.aihaoke.net/api/auth/unified/toAuth
  → 302 se-ars1.gzus.edu.cn/aihaoke/sso.auth?tn=..&ru=..&ck=..&state=..
  → 302 cas.gzus.edu.cn/lyuapServer/login?service=<上面那个>
  → CAS 认 TGT 放行 → 回跳 → passport /api/auth/loginByTicket → haoke-token
```

⚠️ 生产机 DNS 解析不了 `*.gzus.edu.cn`(路由器上游查不到, 但 DoH 能查) →
浏览器要带 `--host-resolver-rules` 映射 IP(见 `scripts/brush_ahk.py`)。
⚠️ passport 的 `locale` 必须 `en-US`(zh-CN 时按钮文案不同, 点不中)。

**任务表**(POST `/api/learn/task/listTask`): 15 个任务, `taskType` 分工:
`10`=微课精讲(视频) / `20`=? / `30`=AI辩论 / `40`=作业 / `50`=小测。
**只刷 taskType=10**; 小测/作业/辩论一概不碰(那是替人做作业)。

**进度怎么记(决定性实验)**:
- 播放时播放器周期性 POST `/api/learn/task/file/updateLearnFile`
  `{classId, taskId, position(秒), overFlag}` + `/api/learn/task/updateLearnTask` 心跳。
- **直接伪造上报没用**: 把 position 拉满、overFlag=1 发过去, 服务器把 overFlag
  **改回 0**、progress 仍是 0 —— 平台按**真实播放**记进度 → 只能真播。
- 而且**必须从任务列表点「To learn」进去**: 直接开播放页 URL, 视频能播完,
  但平台不上报进度(实测 progress 一直 0)—— 学习会话是点按钮时建立的。

脚本: `sidecar/zhihuishu-sidecar/scripts/brush_ahk.py`
定时: `deploy/zhihuishu-ahk-brush.{service,timer}`(**21:00**, 与 19:00 的智慧树
挂机错开 2 小时 —— 这台机器 15W 满载会过热断电, 别让两个浏览器同时跑)。

**记账确认(2026-11-22 实测)**: 从任务列表点进去真播, 播完后
`data/ahk_brush_log.json` 记 `{"taskId": 227778, "progress": 100, "done": 1}`
→ **平台侧确实记账了**(与 polymas 一样是能查到的真进度)。

**踩到的坑: 时限不能按理想速率算**(2026-11-22 夜)
第一轮刷 227777(20.3 分钟的视频)时, 时限按 `时长 ÷ 1.5 + 180s` 算, 结果那一轮
播放**异常掉速到 0.54x**(网络/负载都可能), 16.6 分钟只播到 11.4 分钟就到期 →
**「半途而废」平台不记账**(progress=0 done=0), 等于白播。
修法: 时限按**最坏速率 1.0x** 算(`时长 × 1.15 + 240s`), 视频 `ended` 会提前返回,
这个上限只是兜底。倍速本身是生效的(实测 1.5x: 2 分钟墙钟跑 3 分钟媒体), 但不能
拿它当时限依据 —— 平台随时可能掉速。
另外轮询里每轮补设一次 `playbackRate`(平台播放器有时会重置回 1), 日志会打出实际
生效的 rate。

## 8. 2026-11-22 夜: 把「自动刷完」这件事收口

当晚实跑暴露的三件事, 都已修:

1. **AHK 那条线通了** —— 见上, progress=100/done=1 是平台侧记账, 不是我们自己
   记的账(脚本只是回读)。
2. **polymas 两门课根本没有视频** —— 「26-27-1思想道德与法治」(27 个资源) 与
   「职业生涯规划」(7 个资源) 资源全是 PDF/PPT/DOCX。它们每晚都进目标清单,
   刷课器进课翻一遍资源树再判定「没有待刷的视频」走人, **还占满了 max=3 的名额**,
   把真有视频的「国家安全教育」(progress=60) 挤到第 4 位 → 那一晚根本没刷它。
   修法: 刷课器探到「没有视频」就记一笔(`data/no_video_courses.json`, 带 14 天
   时效, 老师补了视频会自动重新探测), 目标生成器据此排除; `max` 默认 3 → 5。
3. **整轮预算** —— 原来只有「每门 90 分钟」, 5 门就是 7.5 小时, 超过 systemd 的
   `TimeoutStartSec`(4 小时)会被**杀在半路**: 进度只涨一半, 下一轮又从头来。
   现在 `brush_courses.py --total-minutes` 兜住整轮(19:00 那轮 90 分钟),
   每门课时限与整轮剩余取小。

**当晚各条线的真实表现**:
- 19:00 智慧树那轮: ai-smart「程序设计基础」播完 4 个视频(2.1变量 / 3.1运算符 /
  4.1顺序结构 / 5.1选择结构), 45 分钟收工; 两门 polymas 空转(无视频, 见上)。
- 21:00 前后 AHK 那轮: 227778 已在 07:59 刷完(progress=100), 227777 真播 20 分钟。

**定时器时间表(生产机, 用户级 systemd)**:

| 时间 | 单元 | 干什么 |
|---|---|---|
| 08:05 | `zhihuishu-signin-watch.timer` | 白天轮转巡课(签到) |
| 18:45 | `zhihuishu-targets.timer` | 生成刷课目标(分流到三个引擎, 排除无视频的课) |
| 19:00 | `zhihuishu-brush.timer` | 智慧树挂机(整轮 120 分钟, 21:00 前收工) |
| 21:15 | `zhihuishu-ahk-brush.timer` | 学习平台(在线好课)挂机 |
| 23:30 | `zhihuishu-collector.timer` | 作业采集(独占 cookies 续期) |
