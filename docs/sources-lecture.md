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

1. **handler（即时）**：`handle_message` 拦截带音频扩展名文件附件的单聊消息（m4a/mp3/wav/aac/amr/ogg/flac/wma/opus/3gp/aiff/mp2）→ `require_private`（绑定 + 单聊双闸）→ **立即下载**（QQ CDN 链接有时效，不能攒）到 `{workspace}/{msg_id}/raw/` → 写 `lecture.pending` 文档（含回话路由）→ 即时回执。
2. **worker（`schedule run lecture`，默认 60s 一轮）**：取最旧一条 pending → `ffmpeg` 切单声道 16k mp3 片（默认 600s/片，切片目录已有产物则跳过，支持中断续跑）→ ASR 逐片转写拼接 → 按收到时刻匹配当天课表（含该时刻的课优先，否则 2 小时窗内最近一节，匹配不到进 fallback 课程）→ **转写全量内联进 agent prompt**（不落盘、不进 Vault）→ `enqueue_with_notify` 入队 agent 整理笔记写 `{vault_dir}/{课程}/笔记/{日期} {文件名}.md`（幂等键 `lecture:notes:{msg_id}`，终态通知回原会话）→ 立即清理工作区（原始录音+切片删除）→ 状态机 pending → done；failed 终态不自动重试（改完删文档重投）。
**只沉淀整理后的笔记**：转写与音频都是临时产物，Vault 里只有 `笔记/` 目录。
原始文件手机上还有，需要复盘重录即可。

## 配置（ru41.toml）

```toml
[sources.lecture]
# enabled = true            # 缺省启用；false 完全静默
api_key = "GLM 的 key"      # 或 api_key_env = "ZHIPU_API_KEY"（默认）
# asr_base_url = "https://open.bigmodel.cn/api/paas/v4"   # OpenAI 兼容，可换 Groq
# asr_model = "glm-asr-2512"
# slice_secs = 600          # 切片秒数（≥30 才生效）
# workspace = "/data/lecture"
# vault_dir = "01-Courses-课程"   # Vault 内课程根目录（相对路径）
# fallback_course = "课堂录音"    # 课表匹配不到时归入的课程
```

依赖：容器内有 `ffmpeg`；agent（codex 底座）在 mimo-worker 里可写 Vault（笔记由 agent 落盘，worker 本身不碰 Vault）。未配 ASR key 时模块静默缺席（handler 不拦截，schedule list 显示原因）。

## 契约与边界

- `CommandContext.attachments`（plugin-api v0.2.0 新增）：渠道把消息全部附件以 `MediaRef` 列表透传给插件命令；文件 URL 有时效，插件需要落盘就在 handler 里当场下。QQ 渠道的 URL 缺协议前缀，透传时补回 `https://`。
- pending 队列用 TaskStore documents（kind `lecture.pending`），worker 乐观并发更新（revision），重复 poll/进程重启安全；agent 任务幂等键 `lecture:notes:{msg_id}`，重启后重投不重复建任务。
- ASR 客户端是 OpenAI 兼容 `/audio/transcriptions`（multipart，5xx 重试 3 次、4xx 快速失败），换服务商只改三个配置字段。

## 测试

`plugins/lecture/tests/lecture.rs`：非音频不拦截、未绑定/群聊劝退、真下载（本地 mock 文件服务器）+ pending 登记 + 幂等、课表匹配三态（上课中 / 窗口内最近 / fallback）。
