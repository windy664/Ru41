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

- 8-12 个重点词句（「看得懂但听不出、不会主动用」档位 + 视频核心术语，含音标/释义/原句/时间戳）
- 2-3 个实用句式框架（原句 + 用法）
- 1 条听力提示（连读/弱读/语速/术语）
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
- 别名 `/prep`；群聊可用（内容为公开视频，不走身份双闸）。

Agent 任务需要底座网络可用：部署环境若需代理，ru41 进程本身也要带代理环境变量（`proxy_url` 只覆盖 yt-dlp 子进程）。

## 边界

- **只追新，不补旧**：频道首次进入时只登记存量快照（baseline），历史视频一律不预习；之后新出现的视频在 `max_age_days` 窗口内才入队（flat 列表缺发布时间时预习前补一次元数据兜底）。存量好视频用 `/预习` 手动点播。
- 依赖本机安装 yt-dlp（开发机 `pipx install yt-dlp`，版本 2026.10.21 验证通过）与可用代理。
- ASR 自动字幕有转写错误，预习单可能收录「不存在的词」；prompt 要求原句逐字来自转写以降低风险，无法根治。
- 超长视频（播客）转写截断到 `max_transcript_chars`，预习单只覆盖前段内容。
- 每个新视频消耗一次真实 Agent 调用，靠 `max_per_poll` 与关键词粗筛控量。
- 私隐边界：处理的均为公开视频内容，不涉及个人数据脱敏。
