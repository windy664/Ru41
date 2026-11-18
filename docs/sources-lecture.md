---
type: source-adapter
status: implemented
source: 课堂录音（QQ 文件投递）
updated: 2026-10-31
tags: [Ru41, lecture, asr, plugin]
---

# 课堂录音转写（lecture）说明

期末复习向附属：上课录完音，把录音文件直接丢给 QQ 单聊，ru41 当场下载、后台转写，按当天课表归入对应课程目录，再入队 agent 整理成结构化课堂笔记。长期积累的转写+笔记就是复习周的 RAG 素材。代码全部在 `plugins/lecture/`（cdylib 插件，无核心改动依赖）。

## 数据流

1. **handler（即时）**：`handle_message` 拦截带音频扩展名文件附件的单聊消息（m4a/mp3/wav/aac/amr/ogg/flac/wma/opus/3gp/aiff/mp2）→ `require_private`（绑定 + 单聊双闸）→ **当场登记**：写 `lecture.pending` 文档（status=`downloading`，含回话路由）→ **立刻回执**（课程名 + 队列位置）→ **下载甩到插件自己的 runtime 后台跑**，下完把状态翻成 `pending`。
   2026-11-16 之前是**同步下载完才回执**——一节录音几十 MB，家宽到腾讯云 CDN 要 9 分钟，
   整个 QQ 入站线程被堵住：网关心跳停摆掉线，断线窗口里用户发的下一条录音直接丢
   （QQ 侧无消息重放），用户还因为回执排在下载之后而以为「没反应」。现在入站只做登记。
2. **worker（`schedule run lecture`，默认 60s 一轮）**：取**收到时间最早**的一条 `pending`（`downloading` 的跳过——文件还没下完，转写它只会 ffmpeg 失败；见下「队列顺序」）→ `ffmpeg` 切单声道 16k mp3 片（默认 600s/片，切片目录已有产物则跳过，支持中断续跑）→ ASR 逐片转写拼接 → 按收到时刻匹配当天课表（含该时刻的课优先，否则 2 小时窗内最近一节，匹配不到进 fallback 课程）→ **转写全量内联进 agent prompt**（不落盘、不进 Vault）→ `enqueue_with_notify` 入队 agent 整理笔记（幂等键 `lecture:notes:{msg_id}`，终态通知回原会话）——**一门课一份连续笔记** `{vault_dir}/{课程}/课堂笔记.md`，每次课按 `## 日期 主题` 章节追加在末尾，已有内容不动→ 立即清理工作区（原始录音+切片删除）→ 状态机 pending → done；failed 终态不自动重试（改完删文档重投）。
   **下载失败要通知用户**：后台下载没有即时回执兜着，失败会静默——插件走 worker 同款
   「入队 + 通知」通道让 agent 转述「请重新发一次」。卡在 `downloading` 超过 30 分钟的
   （容器重启掐断的）由 worker 兜底判失败并同样通知。
**只沉淀整理后的笔记**：转写与音频都是临时产物，Vault 里每门课只有一份连续
`课堂笔记.md`（章节即时间线），不散落成碎片文件。原始文件手机上还有，需要复盘重录即可。

**磁盘不会越堆越多**（2026-11-16 修完三个漏点后）：
- 转写完成 → 切片立刻删（每片几十 MB，是大头）；
- 原始音频过保留期（`retain_audio_days`，默认 7 天）→ 每轮 poll 扫队列时删；
- 转写全文落进 `lecture.pending` 记录（不只在磁盘上）。
> 这三条曾经都落空过：worker 在 CAS 更新处抛错、从没走到清理那步（切片堆了 1.1G）；
> 转写全文也从来没落过库（29 条记录全是 NULL，后用切片 .txt 补回 15 条）；
> 「过保留期再删」只在转写当下判过一次、age=0 永远不满足，原始录音会永久留着。

## 录音文件名（决定课程归属与归档日期）

- **认录**：音频扩展名（m4a/mp3/wav/aac/amr/ogg/oga/flac/wma/opus/3gp/aiff/aif/mp2），
  或无扩展名但形如「语音/录音/录制/rec/voice + 时刻」——部分录音 app 导出的
  `语音 261116_144234` 这类文件转发后扩展名就没了，也照样当录音收。
- **解析开录时刻**（优先于收到时刻，更贴近真实上课时间）：文件名里的
  `20261103_143000`、`20261103143000`、`2026-11-03 14-30-05`、`Rec_2026.11.03-14.30`、
  `录音 2026年11月3日 14_30`、`2026年11月04日 上午09点11分(1).mp3`、
  `语音 261116_144234`（两位年份，69-99 归 19xx、00-68 归 20xx）。
  只有星期与时刻的（`周四 上午09点01分`）按收到时刻往前回推最近一次。
- 解析不出就回退收到时刻，并在提示词里写明依据（「来自录音文件名」/「文件名无时间」）。

## 队列顺序（先到先转）

- **按收到时间排**，不按文档 id：待办文档的 id 就是 QQ 的 msg_id（一串随机 blob），
  `documents()` 只能按 id 排序，生产库里实测 id 序与收到时间序完全对不上。
  插件把待办拉回来自己按 `received_at` 排（`pending::oldest_unfinished`）。
- **扫描窗口会逐级放大**（200 → 2000 → 20000）：`records` 没有 delete 接口，
  done 的待办记录永远留着，会把固定窗口占满——排在窗口外的录音会永远轮不到，
  队列静默饿死（每轮只回 `{"processed": null}`，不报错也不转写）。
- 连传多条录音时：handler 每条都当场下载落盘并写待办，回执报「前面还有 N 节」，
  worker 串行处理，一轮一条，转写中的新录音排在队列里等下一轮。

## 配置（ru41.toml）

```toml
[sources.lecture]
# enabled = true            # 缺省启用；false 完全静默
api_key = "占位值"          # 或 api_key_env = "MINIMAX_API_KEY"（默认）；whispercpp 本地服务可填占位值
# asr_api = "openai"        # openai（默认，OpenAI 兼容）| whispercpp（whisper.cpp server 原生 /inference）
# asr_base_url = "https://open.bigmodel.cn/api/paas/v4"   # OpenAI 兼容，可换 Groq
# 本地 whisper.cpp server 示例：
# asr_api = "whispercpp"
# asr_base_url = "http://whisper-asr:8080"
# 本地 SenseVoice（推荐，CPU 上快十几倍）：deploy/sensevoice/ 提供了
# whisper.cpp 兼容的 /inference 端点，配置同样用 whispercpp 风格：
# asr_api = "whispercpp"
# asr_base_url = "http://sensevoice-asr:50000"
# asr_model = "glm-asr-2512"
# slice_secs = 600          # 切片秒数（≥30 才生效）
# workspace = "/data/lecture"
# vault_dir = "01-Courses-课程"   # Vault 内课程根目录（相对路径）
# fallback_course = "课堂录音"    # 课表匹配不到时归入的课程
```

依赖：容器内有 `ffmpeg`；agent（codex 底座）在 mimo-worker 里可写 Vault（笔记由 agent 落盘，worker 本身不碰 Vault）。未配 ASR key 时**网关侧照样接管录音**（见下），worker 起不来时 schedule list 显示原因。

## 2026-11-17 踩坑：网关侧没有 ASR key，录音就没人接管

现象：QQ 发了录音，不进课堂笔记，只在 agent 那边收到一句
「请补充具体处理要求（转写/总结/翻译/提取行动项）」。

根因在插件注册的**门控**：

```rust
qq_handlers: |config, _| {
    if !lecture.is_ready() { return Ok(Vec::new()); }   // ← 拿 ASR key 当注册条件
```

`is_ready()` 要求**当前进程**的 env 里有 ASR key。可 ASR key 只在
`ru41-lecture`（真正跑转写的 worker）里，**三个网关（qq / telegram / mimo-worker）
一个都没有** → 网关根本不注册 handler → 录音没人接管 → 消息掉给 agent。

两个连带认知：

- `schedule list` 里那句「缺 ASR key：…」是**网关视角的误报**，worker 其实一直就绪；
- 网关侧只做「下载 + 登记待办」，**压根不调 ASR**，让它为一个用不上的 key 门控是搞反了。

修法（`13687f9`）：

1. `resolved()` 缺 ASR key **不再报错**，解析成空串；
2. `qq_handlers` 只看 `is_enabled()`（模块有没有开），ASR key 缺不缺是 worker 的事，
   仍由 `schedule list` 的 `not_ready_reason` 报。

对照 `ecard` 等插件：qq_handlers 一律无条件注册，就绪状态在 schedule list 报。

**这次那条录音的补救**：QQ 的下载链接当时还没过期（实测 28 MB 还能下），
手动把文件放进 `workspace/<msg_id>/raw/` + 手工建一条 `lecture.pending`（status=pending），
worker 下一轮就接手了 → 转写 3816 字、课程自动匹配「网络空间安全导论」、笔记写进 Vault。
（`accept_recording` 建的待办长这样：`file`/`filename`/`msg_id`/`received_at`/`size`/
`status`/`error`/`course`/`channel`/`target`。）

## 笔记内容规则（提示词里管住的事）

笔记的**形状**由 agent 判断，**但哪些毛病不许犯**是提示词里写死的规则——
2026-11-18 对「国家安全教育」那节做了实测审计，发现同一批内容在正文/问答/题型/清单里
重复 4~5 遍（占 46% 篇幅），还混进了哲学闲聊、举报老师、房价闲谈，
且「华人局」这种待核对的案名被排进了必背清单。于是补了这些规则：

| 规则 | 管住什么 |
| :--- | :--- |
| 3b 同一件事只写一遍 | 题型/易错点/复习清单只在**确有增量**时再提（答题套路、错误做法 vs 正确做法、可直接背的原话），换个说法重述不算增量 |
| 3c 与本课主题和考试无关的内容不写 | 哲学闲聊、课堂纪律与举报、房价八卦、老师个人经历、同学发言——除非老师当场点成考点 |
| 3d 课堂投票不是考场论据 | 投票是教学过程，不是答题材料 |
| 8 待核对的不进清单 | 转写里还「待核对」的内容不进复习清单（等于让人去背可能错的东西），集中放末尾「待核对」小节 |
| 1b 同名章节已存在就替换 | 重跑同一节课或录音重发时，**替换**那个章节，不要在末尾再挂一个同名章节 |

> **1b 是踩出来的**：2026-11-18 用新提示词重跑同一节课，agent 看到
> `## 2026-11-18 语音 261118_170347` 已经在文件里，判定「内容已覆盖」整节不动——
> 旧版本那些重复和闲谈原样留着，重跑等于白跑。补了 1b 之后重跑才真的替换
> （Vault commit `b276ae79` → `f1b5138e`）。

重跑那一节的量化对比（同一条转写，6058 字）：

| | 旧版 | 新版 |
| :--- | :--- | :--- |
| 正文字数 | 6116 | 3721（**−39%**） |
| 「一刀切」/「传导链条」/「发展才是硬道理」 | 9 / 4 / 4 次 | 5 / 0 / 2 次 |
| 闲谈残留（哲学/迟到/举报/掏窃/房价/651） | 10 类 | 0 类 |
| 「待核对」 | 散落 13 处 | 集中 5 条，末尾单列一节 |
| 小标题数 | 14 | 12（去掉跑题的「就业与活人的路」） |

重跑怎么做（不改代码）：往 `lecture.pending` 插一条新文档（新 `msg_id`，
**旧的幂等键不能再用**——提示词变了，同键会被 `enqueue_for` 判成「同键不同 prompt」而报错），
工作区里放 `slices/slice000.mp3` + `slices/slice000.txt`（转写全文），
worker 的 `transcribe()` 会命中 txt 缓存**跳过 ASR**，直接用新提示词入队 agent。

## 契约与边界

- `CommandContext.attachments`（plugin-api v0.2.0 新增）：渠道把消息全部附件以 `MediaRef` 列表透传给插件命令；文件 URL 有时效，插件需要落盘就在 handler 里当场下。QQ 渠道的 URL 缺协议前缀，透传时补回 `https://`。
- pending 队列用 TaskStore documents（kind `lecture.pending`），worker 乐观并发更新（revision），重复 poll/进程重启安全；agent 任务幂等键 `lecture:notes:{msg_id}`，重启后重投不重复建任务。
- ASR 客户端是 OpenAI 兼容 `/audio/transcriptions`（multipart，5xx 重试 3 次、4xx 快速失败），换服务商只改三个配置字段。

## 测试

`plugins/lecture/tests/lecture.rs`：非音频不拦截、未绑定/群聊劝退、真下载（本地 mock 文件服务器）+ pending 登记 + 幂等、课表匹配三态（上课中 / 窗口内最近 / fallback）。
