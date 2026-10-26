---
type: source-adapter
status: implemented
source: YouTube 频道（yt-dlp）
updated: 2026-10-24
tags: [Ru41, YouTube, 英语学习, source]
---

# YouTube 英语预习说明

英语学习附属：轮询订阅频道的新视频 → yt-dlp 拉英文字幕转写 → Agent 合成「重点词句预习单」→ 终态经 `[notify]` 默认出口推送。预习单与视频链接同条消息，先学词再看片。代码：`src/modules/youtube/`（`source.rs` yt-dlp 子进程客户端、`service.rs` 去重与逐视频入队、`config.rs` 配置段）。

## 与 rss / papers 的差异

- **不用 RSS**：`youtube.com/feeds/videos.xml` 对本部署 404（代理与直连均验证，2026-10），采集与字幕统一走 yt-dlp 子进程。`--flat-playlist --print` 拿列表（newest first），`--write-sub` 优先手动字幕、缺省退回 `--write-auto-sub`（ASR）。
- **逐视频任务而非合并 digest**：每个新视频独立入队一个预习任务（幂等键 `youtube:prep:<video_id>`），因为每个视频都要先拉转写再生成预习单。
- **seen 状态机**（records kind `youtube_seen`，键 `{channel_url}#{video_id}`）：`pending`（转写失败重试，最多 3 次）→ `done`（已入队）/ `failed`（放弃）；`filtered`（关键词未命中）/ `short`（时长 < `min_duration_secs`，默认 60s，过滤 shorts）为终态跳过。
- 推送文本即 Agent 输出原文（runtime/dispatcher 的 `result.summary`），因此视频链接要求 Agent 原样写进预习单第二行。

## 预习单口径

- **现场装备 4-6 个**：按长期迁移价值排序，前 2-3 个为 ⭐ 必学（工作/技术/日常依然高频，标美式 IPA），其余为场景词；每条带情境释义 + 完整原句 + 逐字时间戳。
- **带走就用 2-3 个**：可迁移口语块，附大学生生活例句。
- **听力锚点 2-3 处**：完整原句 + 具体音变（哪个音→哪个音）+ 8 类现象标签（落库供发音专题）。
- **听前自测 1 题 + 看后检测 2-3 题**：检测含理解选择/⭐表达填空/中译英，答案一行放消息末尾。
- **原句完整性（硬性）**：所有引用必须逐字出自转写、且为完整句子或语义完整片段（可跨字幕行拼接），禁止半句截断。
- 时间戳策略：转写每行带 `[Ln m:ss]` 标记，prompt 硬性要求逐字照抄、禁止自行换算；模型仍可能漂移，**代码级反查锚定（引用句回转写定位）留给 v2**——通知路径推送 `result.summary` 原文，附属无后处理钩子。

## 配置（ru41.toml）

```toml
[notify]
channel = "qq"

[sources.youtube]
channels = ["@Fireship"]            # @handle 或频道 URL，自动归一化为 /videos 列表页
keywords = []                       # 可选标题粗筛；缺省不过滤
interests = "计算机 / AI / 安全"      # 注入预习单 prompt
level = "CET-6 / B2"                # 学习者水平描述，注入 prompt
min_duration_secs = 60              # 过滤 shorts
max_per_channel = 15                # 每频道单轮检查的最新视频数
max_per_poll = 2                    # 单轮最多入队任务数（控制 Agent 额度）
max_age_days = 14                   # 新鲜度窗口：超窗老视频只登记不预习；0 关闭窗口
max_transcript_chars = 24000        # 注入 prompt 的转写上限（长视频截断）
interval_secs = 21600               # youtube run 轮询间隔，默认 6 小时
digest_at = "12:10"                 # 每日日报推送时刻（本地 HH:MM），默认 12:10
notify = true
ytdlp_bin = "yt-dlp"                # 可执行文件路径
proxy_url = "http://127.0.0.1:7890" # 传给 yt-dlp 子进程；缺省继承 ru41 进程环境
```

## 运行

```bash
cargo run --locked -- youtube poll        # 单轮（适合系统 cron）
cargo run --locked -- youtube run         # 常驻周期轮询
cargo run --locked -- youtube prep <url>  # 手动点播：单个视频立即生成预习单
ru41 schedule list                        # 确认 youtube 任务已登记
```

## QQ 命令

- `/预习 <YouTube链接>`：单视频点播，支持 watch / youtu.be / shorts 链接或 11 位裸 ID。拉字幕转写后入队预习任务，终态推送到当前会话；同一视频重复提交幂等返回既有任务。非视频链接不拦截（消息继续走 Agent 通道）。
- `/复习 <不会的编号>`（如 `/复习 2 4`，其余记会；全部都会回 `/复习 all`）：对最近一次复习推送反馈。别名的 `/review` 同义。
- `/定级`：英语水平定级测试（3 句英译中，难度递增）；收到题目后回复 `/定级 <三句翻译>` 交卷，评测结果推送并自动调整后续预习难度。
- `/预习` 别名 `/prep`、`/定级` 别名 `/level`；群聊可用（内容为公开视频，不走身份双闸）。

Agent 任务需要底座网络可用：部署环境若需代理，ru41 进程本身也要带代理环境变量（`proxy_url` 只覆盖 yt-dlp 子进程）。

## 日报闭环（youtube-review job）

预习单只解决「看懂」；学习发生在看之后。**所有推送打包成每天一条「英语日报」**（`digest_at`，默认 12:10），其余时间零打扰。一条日报可能包含：

1. **新预习单**：自动轮询发现的新视频（最多 2 期/日，余量顺延）；手动 `/预习` 不等日报、即时推送。
2. **词块复习**：到期词块（1/3/7/14/30 天间隔阶梯，每视频单日最多 2 条、单批最多 8 条）——中文线索回忆英文块；回复 `/复习 <编号>` 标记不会（次日再推），3 天不反馈自动按会结转，走完阶梯毕业为 known。
3. **发音专题**：听力锚点按语音现象（连读/弱读/闪音/失爆/同化/缩读/重音）落库归类，同现象攒够 3 条推一期专题课，例句全是看过的视频原句；每个日历日至多一期。

学习机制：

- **学习者模型**：known 词块注入预习 prompt 排除名单；`/定级`（3 道英译中 → Agent 评测 CEFR 级别）写入 `youtube_level`，预习挑词难度随之调整，配置里的 `level` 仅作兜底。
- job 15 分钟一跳，仅在日报窗口内触发一次；提取、定级回收随时进行。

records 数据模型：`youtube_vocab#<video>#<n>`（词块）、`youtube_review_batch#<日期>`（当日复习批，`/复习` 反馈键）、`youtube_anchor#<video>#a<n>`（听力锚点）、`youtube_anchor_topic/latest`（专题当日已推标记）、`youtube_digest#<日期>`（日报已发标记与所含预习单）、`youtube_placement/latest` 与 `youtube_level/latest`（定级状态与结果）。生产以 compose `youtube` / `youtube-review` 服务（profile `youtube`）常驻。

## 边界

- **只追新，不补旧**：频道首次进入时只登记存量快照（baseline），历史视频一律不预习；之后新出现的视频在 `max_age_days` 窗口内才入队（flat 列表缺发布时间时预习前补一次元数据兜底）。存量好视频用 `/预习` 手动点播。
- 依赖本机安装 yt-dlp（开发机 `pipx install yt-dlp`，版本 2026.10.21 验证通过）与可用代理。
- ASR 自动字幕有转写错误，预习单可能收录「不存在的词」；prompt 要求原句逐字来自转写以降低风险，无法根治。
- 超长视频（播客）转写截断到 `max_transcript_chars`，预习单只覆盖前段内容。
- 每个新视频消耗一次真实 Agent 调用，靠 `max_per_poll` 与关键词粗筛控量。
- 私隐边界：处理的均为公开视频内容，不涉及个人数据脱敏。
