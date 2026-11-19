---
type: channel-adapter
status: implemented
channel: Telegram Bot API
updated: 2026-11-15
tags: [Ru41, Telegram, channel, notify, gateway]
---

# Telegram 渠道说明（方案 + 实现记录）

> **阶段 1（单向下发）与阶段 2（双向网关）均已实现**（提交 `f5ab03e`）。
> 核心代码在 `src/channels/telegram/`；QQ 全功能参考 `src/channels/qq/`，单向通知参考 `src/channels/feishu.rs`。
> 结论：Telegram 接入在 Ru41 现有架构下可行，**自研轻量适配器**（零新依赖），**接收侧长轮询**，**生产部署必须走 mihomo 代理**（实测直连不通）。

## 实现状态（2026-11-15）

**阶段 1：单向下发**

| 文件 | 内容 |
|---|---|
| `src/channels/telegram/api.rs` | Bot API 客户端：`getMe` / `sendMessage` / `sendPhoto` / `sendVideo` / `sendVoice` / `sendDocument`；token 只以脱敏地址出现在日志与错误里 |
| `src/channels/markdown.rs` | **渠道无关的渲染层**（`RenderProfile`）：通用 Markdown → 目标方言；标题/表格/公式按 profile 降级；QQ 与 Telegram 各一份 profile |
| `src/channels/telegram/mod.rs` | `TelegramNotifier`（`NotifyChannel`）：target 解析（`default` / `chat:{id}` / 裸数字 / @username）、inline keyboard、媒体发送、排版失败降级纯文本 |
| `src/ingress/cli_ops/telegram.rs` | `ru41 telegram doctor`：只调 `getMe`，不发送消息 |
| `tests/telegram_notify.rs` | 7 个用例：HTML 排版、target 解析、**token 不泄露**、排版失败降级、媒体、dispatcher 终态投递 |

**阶段 2：双向网关**

| 文件 | 内容 |
|---|---|
| `src/channels/builtin.rs` | **渠道无关的内置命令层**：只有一份词表（`builtin::DEFAULT`），QQ 与 Telegram 走同一套解析与执行；help 的入口是「/」 |
| `src/channels/telegram/bot.rs` | `TelegramBot`：长轮询（offset 持久化到 `records` 表 `telegram/offset`）、幂等入队（`telegram:msg:{id}`）、私聊打断、内置/扩展命令、按钮回调（`callback_query` 代发命令）、轮询存活探针（`pending_update_count` + 卡死时长）、409 冲突退避、用户白名单 |
| `tests/telegram_gateway.rs` | 9 个用例：入队路由、幂等、命令回复、白名单、轮询消费与 offset 落库、**按钮回调**（应答→执行→回复带 inline keyboard）、群里按钮不越权、非白名单点击忽略、待办回执落成「纯符号 + 底部按钮」 |

**跨渠道改造**（影响 QQ，但行为保持）：

- `crates/ru41-plugin-api/src/ports/commands.rs`：`msg_request_key(channel, msg_id)` 渠道化（原写死 `qq:`）。
- `src/bootstrap/assembly.rs`：`qq_command_handlers` → `channel_command_handlers`；插件命令按契约只用 `ctx.reply_channel`，因此同一批处理器原样注册给 QQ 与 Telegram。
- `src/runtime/dispatcher.rs`：`effective_msg_id` 按渠道分支——Telegram 无被动回复窗口，`msg_id` 长期有效（当 `reply_parameters` 用）。
- `src/runtime/worker.rs` + `src/markdown.rs`：平台提示不再按渠道注入——`OUTPUT_STYLE` 统一写「不要用表格」，各渠道的降级由渲染层 profile 负责。

**配置**：`[channel.telegram]` = `bot_token` / `default_chat_id` / `bind_secret` / `allowed_user_ids`；环境变量 `RU41_TELEGRAM_BOT_TOKEN` / `RU41_TELEGRAM_BIND_SECRET`。

**CLI**：`ru41 telegram doctor` / `send` / `run`（`run` = 长轮询网关 + 通知派发）。

**实测结果（真实 bot @Ru41bot）**

- `ru41 telegram doctor` → `bot @Ru41bot (id 9269761012)`，token 不出现在输出里。
- `ru41 telegram send --chat-id …` → 真实送达，标题/粗体/列表/代码/链接渲染正确。
- `[notify] channel = "telegram"` → 任务落库为 `channel=telegram / target=default`（**chat_id 不落库**）→ dispatcher 把终态结果推到真实 Telegram，`notified_at` 已写入。
- `ru41 telegram run` → **真实长轮询**：收到用户回复后入队（`telegram:msg:10`，内容「收到」），offset 落库为 `357245866`，同会话未完成的旧任务被**打断并取消**（私聊打断语义生效）。

**镜像**：`ru41:local` 已用最终 HEAD 重建（主二进制 + 24 个插件 .so），镜像内 `ru41 telegram doctor` / `run` 子命令可用。

## 生产部署（192.168.1.223）

生产用的是**裸 `docker run`**（不是 compose），Telegram 网关容器规格与 `ru41-qq` 同构：

```bash
# 凭据与代理放 0600 env 文件（token 绝不出现在命令行/进程列表）
/home/windy2/.ru41-telegram.env   # RU41_TELEGRAM_BOT_TOKEN / RU41_TELEGRAM_BIND_SECRET
                                    # + HTTP_PROXY/HTTPS_PROXY=http://172.17.0.1:7890 + NO_PROXY
                                    # + 从 ru41-qq 动态取的 RU41_JWXT_*（library 插件需要）

docker run -d --name ru41-telegram \
  --network ru41-net --restart unless-stopped \
  --env-file /home/windy2/.ru41-telegram.env \
  -e CODEX_HOME=/data/codex \
  -v /home/windy2/ru41-mimo-smoke/skills:/usr/share/ru41/plugins:ro \
  -v /home/windy2/ru41-mimo-smoke/plugins:/etc/ru41/plugins \
  -v /home/windy2/ru41-mimo-smoke/data:/data \
  -v /home/windy2/ru41-mimo-smoke:/etc/ru41 \
  ru41:local --config /etc/ru41/ru41.toml telegram run --backend pi
```

生产配置里 `[channel.telegram] default_chat_id = "8787916999"`（主动通知的默认目标；聊天消息的目标由消息本身决定，不需要它）。
`[notify].channel` 仍是 `qq`（主渠道），`[notify].fanout = ["telegram"]` → **后台简报/日报/提醒已经双发**（见下）。

### 消息渲染：规范一份 + 网关各适配

Telegram 不是「再写一套 Markdown」——**规范层是渠道无关的，渲染层按网关方言适配**：

| 层 | 位置 | 职责 |
|---|---|---|
| 规范层 | `src/markdown.rs`（`OUTPUT_STYLE`）+ `src/util/msg::Card` | 模型只填内容，不填版式；`Card` 版式落在所有 bot 平台的 Markdown 交集里 |
| 渲染层 | `src/channels/markdown.rs`（`RenderProfile`） | 通用 Markdown → 目标方言；**表格/公式统一降级**（所有平台都不支持） |
| 网关层 | `src/channels/telegram/` | 只负责投递（Bot API 调用 + 失败兜底） |

Telegram 的 profile：`markup=Html`、`supports_headings=false`（标题降级粗体）、`supports_tables=false`、`supports_latex=false`、`max_len=4096`。
QQ 的 profile：`markup=Markdown`（平台自己解析）、标题保留、同样不支持表格/公式。

**踩过的坑（两处）**：

1. 帮助页早先写的是 Markdown 表格，而**没有平台支持表格**（QQ 的 markdown 子集也不支持）→ 两端都渲染成一堆竖线。现在表格一律降级成条目列表，帮助页改走 `Card` 版式。
2. 待办清单早先把 QQ 专有的 `###`（拿标题层级当字号）与 `<qqbot-cmd-input>`（行内可点复选框）
   直接写进正文 → 在 Telegram 上渲染成一坨标签文本。现在规范层只产出中性占位符
   `{{cmd:符号|指令}}`，`channels::qq::render` 是**唯一一处** cmd-input 适配：
   QQ 展开成行内可点标签并放大字号，其它渠道降级成纯文本符号。

**命令输入不分渠道**：内置命令只有一份词表（`channels::builtin::DEFAULT`），
QQ 与 Telegram 走同一套解析与执行；help 的入口是「/」，**没有 `/help` 这个命令**。

### 待办复选框：动作走渠道无关的 Reply 按钮

待办清单的动作**不写进正文**，而是放在 `Reply.markup`（渠道无关的 `ReplyMarkup`：label + 命令文本）：

| 渠道 | 正文 | 动作落点 | 交互 |
|---|---|---|---|
| QQ | 展开成行内 cmd-input 芯片 | 正文里（`has_inline_actions()` 命中 → **不挂底部键盘**，避免两个可点入口） | 两跳：点芯片 → 填输入框 → 发送 |
| Telegram | 芯片降级成纯符号 `☐`/`▣` | 底部 inline keyboard（`callback_data` = 命令文本） | **一跳**：点一下直接完成 |

- 插件零改动：回调执行的仍是同一条命令文本（`/完成 <id>`、`/恢复待办 <id>`）。
- 回调的私聊判定看**会话类型**（`chat.type == "private"`）——群里的按钮同样属于群聊，私人数据闸门不能被绕过。
- 回调的 `callback_query` 会先 `answerCallbackQuery`（否则 Telegram 按钮一直转圈），再执行命令。

### ⚠️ 复选框符号：已勾选那个必须用「非 emoji 字符」（2026-11-19 实测三轮）

Telegram 的字体和 emoji 支持比 QQ 全，**同一个清单里的两个复选框会长得不一样**——
不是渲染 bug，是 Unicode 属性导致的：

| 符号 | 码位 | 在 Unicode 18.0 的 `emoji-data.txt` 里 |
|---|---|---|
| `☐` | U+2610 | **不在** emoji 列表 → 永远是纯文本字形（细线方框） |
| `☑` | U+2611 | **在** emoji 列表（`2611 ; Emoji`）→ 客户端有 emoji 字体就按 emoji 呈现 |

于是未勾选是蓝色细线框、已勾选是**灰色大勾框**（emoji 字体接管了，链接主题色也失效），
看着像两种东西——这就是要修的那个问题。

**试过两条路**：

1. `☑` + **U+FE0E（VS15）**，明确请求按文本呈现 → **Telegram 不认**，
   实测发出去还是那个大 emoji 勾框（VS15 对这个字符无效，emoji 字体照样上）。
2. ✅ **U+25A3 `▣`**（白方框里一个实心小方块）：它**根本不在 emoji 列表里**
   （既无 emoji 变体，也无 emoji 呈现），任何客户端都只能按纯文本字形渲染，
   和 `☐` 绝对一致。同族（都是方框），所以是一套。

> 同类备选（都是非 emoji 字符）：`✓` U+2713、`▢` U+25A2、`☒` U+2612。
> `✓` 不带框，与 `☐` 不同族，没选；`☒` 读起来像「取消」而不是「已完成」。
> 要注意 `✔` U+2714 **是** emoji 字符（只有 `✓` U+2713 不是），别选错。

符号常量集中在 `plugins/planner/src/view.rs` 的
`UNCHECKED_MARK` / `CHECKED_MARK`（改符号只改这一处），
底部按钮标签（`qq_command.rs`）引用同一份常量，不再各写一份字面量。

> ⚠️ **改完符号必须重启三个容器**：`ru41-qq`、`ru41-mimo-worker`、`ru41-telegram`。
> 插件 `.so` 是**进程启动时加载**的——只换文件不重启，某个渠道就还是老样子。
> （2026-11-19 在这栽过：换了 `.so` 只重启了 qq 和 worker，TG 端发的还是旧符号，
> 差点把已经改对的东西当成「没生效」又倒回去一轮。）

> 经验：**判据不是「长得像不像」，而是「在不在 emoji 列表里」**。
> 只要字符有 emoji 呈现的可能，换客户端就可能变色——只有非 emoji 字符才是跨客户端一致的。

### Telegram 的正文可点：深链复选框（2026-11-16 加）

Telegram **没有「正文内嵌按钮」**这种能力——正文里能点的只有链接。所以正文里的复选框
用 `tg://` 深链实现「复选框长在文字里」：

```
tg://resolve?domain=ru41bot&start=<base64url(命令文本)>
```

- 点击后 Telegram 会把 `/start <payload>` 发给机器人（**以你的名义**）——
  这条机器消息处理完会被**删掉**，聊天记录里不留痕；回复也不挂回复锚点
  （锚着一条马上要删的消息会显示「原消息已删除」）。
- payload 用 base64url 编码：Telegram 的 start 参数只允许 `A-Za-z0-9_-` 且 ≤64 字符，
  中文命令必须编码；超过 64 字符就退回纯符号（不留空 href）。
- 网关的 `/start <payload>` 解码后**按用户输入的命令执行**——身份白名单与私聊闸门照旧；
  普通 `/start`（无 payload）仍是欢迎语。
- bot 用户名用 `getMe` 取一次并缓存；只有正文里确实有可点指令时才去取（不额外打扰平台）。

**两套入口的分工**：正文深链是**主入口**（复选框长在文字里，点一下直接完成、
不留痕）。底部 inline keyboard **只在深链造不出来时兜底**（拿不到 bot 用户名、
payload 超长）——正常情况下不挂，同一个动作两个入口是噪音。

> 注意：**没有**正文可点指令的回复（比如内置命令的任务列表「详情/取消」）仍然挂底部键盘
> ——那类动作在正文里没有对应物，不属于重复入口。

⚠️ **清单回执必须走同一条产出路径**：裸 `#`（列清单）与 `#内容`（记一条后回清单）都要用
`todo_list_reply`——早先裸 `#` 走的是重构前的旧写法（只回正文、不挂 markup），
于是 Telegram 上「没看到按钮」，退化成两跳。

### 参考实现：hermes-agent 的 Telegram 层（2026-11-16 调研）

NousResearch/hermes-agent（★26 万）的 telegram 适配层有 **60 个模块**
（`plugins/platforms/telegram/`），比这边完整得多。逐个读过之后，
**已采纳**下面这些（都带回归测试）：

| 借鉴点 | 他们的做法 | 我们的落地 |
|---|---|---|
| 围栏代码块是原子单元 | 切块时 fence 闭合重开（带语言）、表格重复表头 | `channels::markdown.rs`：围栏整体渲染成 `<pre>`，切分时用同一语言标注重开 |
| 长度按 UTF-16 码元 | emoji/CJK Ext B 占 2 单位，`len()` 会低估 | `utf16_len()` + 分片按码元算（`CHUNK_BUDGET` 留余量） |
| 表格降级有章法 | `**行标题**` + `• 列: 值`，不删列 | 我们降级成「一行 · 一行」条目（`Card` 版式一致性优先） |
| 只允许白名单标签 | 白名单外全转义 | 我们正文统一转义 `& < >`，URL 额外转义 `"`（属性） |
| 占位符 stash | 先成型的片段存住，最后统一回填 | 已有（行内代码/链接先 stash，避免被后续替换二次破坏） |
| 每会话发送槽 | send 与 edit 共用 1s 槽；429 冷却窗 fail closed | `ChatGate`：每 chat 最小间隔 1.1s + 429 冷却；**进程级共享**（网关回复与投递推送是两条路，发的是同一个会话，各自一份节流器就会各发各的） |
| 限速错误分类 | REFUSED/FLOOD/UNSENT/LOST 分开处理；LOST 绝不换形重发 | `PlatformError` 带 `error_code`/`retry_after`；429 短等待就地退避，长等待交给投递层改期 |
| 长惩罚不 inline sleep | >5s 交上层 ledger，避免钉死协程 | 同上（`MAX_INLINE_FLOOD_WAIT = 5s`） |
| 幂等键不能只靠 message_id | 用 `bot_id:update_id`（message_id 在群聊里不全局唯一） | 键改成 `telegram:msg:{chat_id}:{message_id}` |
| 按钮状态用 pop 消费 | 第二次点返回「已处理」 | 未采纳：我们的动作是幂等命令（`/完成 <id>`），重复点只会得到「已完成」 |
| 卡死探针用 `getWebhookInfo` | 不抢 update 的探针 | 已有（我们本来就是 `getWebhookInfo().pending_update_count`） |
| 代理在场则禁用 IP fallback | 有代理就不走直连 IP 兜底 | 天然一致（我们只走 `channel.telegram.proxy_url`） |

**明确没抄的**：Topic/会话绑定、媒体相册、草稿流式编辑、inline query 选择器、
DoH + IP 字面量兜底、连接池代退休——这些是**多用户/群组/大媒体**场景的需求，
当前是单人私聊 + 纯文本，投入产出不划算。

⚠️ **用户/模型正文不得携带占位符**：可点动作只该由插件的结构化动作产生。
待办标题、模型输出进入规范文本时，`{{cmd:…}}` 会被作废（插一个零宽空格，
肉眼不可见），否则用户写一段 `{{cmd:☐|/取消待办 x}}` 就能伪造出一个可点动作。


### 双网关但不双通知

| 消息来源 | 任务 `notify.channel` | 投递到 |
|---|---|---|
| QQ 里发的 | `qq` | 只回 QQ |
| Telegram 里发的 | `telegram` | 只回 Telegram |
| 后台简报/日报/提醒 | 取全局 `[notify].channel`（现为 `qq`） | 主渠道 + `[notify].fanout` 里的并行渠道 |

**双投（fanout）**：`[notify] fanout = ["telegram"]` 让**主动推送**（路由无 `msg_id` 的任务）同时发多个渠道；聊天任务带 `msg_id`，只回原会话，不双发。

⚠️ **提醒类消息曾经不双投**：`fanout` 原本只在 dispatcher 里生效，而到点提醒、行程提醒、
错过的课程提醒是**直接调渠道发的**（`notifier_for_route` → 单个渠道），只到 QQ；
提醒容器也没带 telegram token。现在：

- 新增 `ports::channel::ProactiveChannel` 端口 + `runtime::proactive`：
  主动推送**按渠道分别记账**（与 dispatcher 同一套 `delivery` 文档与 CAS 写法），
  某个渠道失败重试时已送达的渠道不重发，同一 `delivery_id` 幂等；
- 三个提醒函数改走该投递口，各自带稳定的投递 id（该轮最早那条的时间戳）；
- **容器必须带凭据**：`ru41-todo-remind` / `ru41-class-remind` 现在也带
  `RU41_TELEGRAM_BOT_TOKEN`（从 `/home/windy2/.ru41-telegram.env` 读，写在
  `.roll/specs.json` 的容器规格里）——没有凭据就发不出去。

实测（2026-11-16）：造一条到期待办 → `todo remind --once` → 记账
`proactive:todo:<anchor>` 的 `sent = {"qq": true, "telegram": true}` ✓。

投递按**渠道分别记账**（`delivery` 文档里的 `sent` 映射）：某个渠道失败重试时，已送达的渠道不会重发；全部送达才标记任务已通知。

两道保险：① 每个容器只注册**自己**的渠道（QQ 容器没有 telegram token，Telegram 容器没有 QQ 凭据），dispatcher 找不到对应渠道就跳过；② 两个 dispatcher 同时盯上同一任务时，`delivery` 文档用 revision CAS + owner 抢占，只有一个能拿到发送权。

⚠️ **双投需要发起方容器也带 telegram token**（生产上放在 `ru41-qq` 的 env 里）：主渠道的 dispatcher 才是这份任务的唯一所有者，由它负责两个渠道的投递。

### 身份绑定

绑定记录：`records` 表 `kind=identity`，键 `{channel}:{openid}`（QQ 是 `qq:{openid}`，Telegram 是 `telegram:{user_id}`），两条独立记录、同一个口令。
口令走 `RU41_TELEGRAM_BIND_SECRET`（复用生产的 `channel.qq.bind_secret`，不落配置明文）。

### 整体滚动与回滚

生产 26 个容器用 `/home/windy2/ru41-mimo-smoke/.roll/` 下的脚本管理：

- `apply.py`：按抓取的规格（cmd / env / 挂载 / 网络 / extra_hosts / restart）逐字重建全部容器 + 换插件；备份旧插件到 `plugins.bak-<时间戳>/`
- `rollback.py`：恢复旧插件 + 用 `ru41:prev` 重建旧容器

⚠️ **插件 ABI 纪律**：宿主与插件必须同版本。给 `ChannelSection` 加字段、改 `msg_request_key` 签名这类**边界闭包内**的公开类型变更，必须 bump `ru41-plugin-api` 的 crate 版本（0.6.0 → 0.7.0 → 0.8.0），否则握手版本号仍相等、照常放行，但结构布局已错 → **SIGSEGV 崩溃循环**（实测 exit 139）。

**更稳的做法已经加上了**：握手多一道 `LAYOUT_STAMP` 硬门——`ru41-plugin-api` 源码的摘要（build.rs 计算）。边界源码一变，摘要就变，旧插件被**明确拒绝**并给出可读原因（「边界布局不匹配：插件 X / 宿主 Y」），不再依赖「记得 bump」这种纪律。2026-11-15 实测：换插件后旧 .so 被干净拒绝（exit 1 + 明确日志），而不是 SIGBUS 崩溃循环。

⚠️ **换宿主二进制必须同步换插件 .so**：生产容器的 `/etc/ru41/plugins` 是宿主目录挂载，**镜像里自带的 .so 会被遮住**。每次宿主二进制升级后，要么把新 .so 同步到宿主插件目录，要么明确知道「旧 .so 与新宿主不兼容」。

## 已知限制

- **媒体消息暂不支持**：Telegram 的图片/文件只给 `file_id`，取直链需要带 token 的 URL（会把密钥暴露进 prompt），需下载后再转存才能给 agent。网关对非文本消息回一句说明。
- **插件命令是中文词**（`/精读`、`/课表`、`#`）：直接发文本仍能命中，但不会出现在 Telegram 的命令菜单里（Telegram 命令名只允许拉丁字母数字下划线）。要做 ASCII 别名需在插件 API 上加别名机制。
- **图片消费者（学习通二维码签到等）只在 QQ 侧接线**：Telegram 侧未接 `ImageConsumer`。
- **`ModuleDef.qq_handlers` 字段名仍是 QQ**：功能上已渠道无关（同一批 handler 注册给两个渠道），字段重命名要动 24 个插件 crate，留作后续清理。
- **单实例**：同一 token 只能有一个消费者轮询（两个实例会互相抢 offset，Telegram 返回 409）。
- **限流只做到第一层**：有每会话最小间隔与 429 退避，但**冷却窗没有落盘**——
  网关重启会忘记上一次的罚时（hermes 把冷却窗持久化，重启不遗忘）。
- **没有「可能已投递」的不确定态**：超时/连接重置这类「请求没拿到响应」的情况，
  投递层仍按失败重试，可能重复发一条（hermes 会标 `delivery_uncertain` 且绝不换形重发）。
- **按钮没有「已处理」反馈**：重复点同一条待办会得到「已完成」而不是「刚才已处理」。

---

## 一、结论速览

| 问题 | 结论 |
|---|---|
| 能不能接 | 能。Telegram Bot API 是简单 REST + JSON，与 Ru41 的 `NotifyChannel` / `CommandHandler` 契约天然匹配 |
| 用现成库吗 | **不推荐**。`teloxide` 0.17.0（2025-07-11）体量大、抽象风格与 Ru41 冲突；`rust-telegram-bot` 0.0.1 已废弃（142 行代码、2023 年停更） |
| 收消息方式 | 默认 `getUpdates` 长轮询（无需公网入口）；`webhook` 作为可选（二者互斥） |
| 最大的坑 | **mihomo 一直在跑且健康，生产机 223 就在规则里（`🇺🇸 美国专用`）**；卡点只是「容器流量没进 mihomo」——配置无 tun 段（无透明代理）+ 系统 DNS 被污染。Telegram 容器注入 `HTTP_PROXY/HTTPS_PROXY=http://172.17.0.1:7890` 即可，分组选节点交给现成规则 |
| 改造量 | 阶段 1（单向通知）≈ 150 行 + 5 处接线；阶段 2（双向网关）≈ 700–900 行，照抄 QQ 结构 |
| 是否影响现有渠道 | 不影响。新增文件 + 新增配置节，`qq` / `feishu` 路径零改动，可单点回滚 |

---

## 二、Telegram 平台约束（2026-11-15 核实）

依据官方文档 [Bot API](https://core.telegram.org/bots/api)（当前 **10.4**，2026-11-09 发布）与 [Bot Features](https://core.telegram.org/bots/features)。

### 协议与收消息

- 请求形式固定：`https://api.telegram.org/bot<token>/METHOD_NAME`，支持 GET/POST，参数可用 JSON、表单或 multipart。
- 响应恒为 `{"ok": bool, "result": ..., "description": ..., "error_code": int}`——与 Ru41 飞书适配器的 `code != 0` 判断写法不同，需另写解析。
- 收消息两条路，**互斥**：
  - `getUpdates`：长轮询，offset 语义，服务端最多保留 24 小时未读更新；
  - `setWebhook`：需要公网 HTTPS 入口 + `secret_token` 校验。
- 同一 bot **只允许一个消费者**轮询（不像 QQ 可以多实例互踢）；两处同时 `getUpdates` 会互相抢 offset。

### 与 Ru41 需求直接相关的平台特性

| 特性 | 事实 | 对 Ru41 的影响 |
|---|---|---|
| 被动回复窗口 | **无**。`sendMessage` 任何时候都能发 | 比 QQ 简单： dispatcher 里 QQ 的 240 秒被动窗口逻辑（`src/runtime/dispatcher.rs:465`）对 Telegram 不适用，`msg_id` 可长期当 `reply_to_message_id` 用 |
| 隐私模式 | **默认开启**：群里只收到「明确 @ 本 bot 的命令 / 回复 bot 的消息 / bot 自己的消息」；私聊、服务消息始终收到 | 群内自然语言消息默认收不到。要么群里统一用 `/命令`，要么 BotFather `/setprivacy` 关闭（官方明确不推荐） |
| 主动私聊 | bot **不能**给未启动过对话的用户发消息；用户必须先 `/start` | 通知出口需要用户先与 bot 建立私聊 |
| 加群 | bot 需被拉进群；`/setjoingroups` 可禁止 | 群场景需邀请 |
| 频控 | 约 30 条/秒全局；单群约 20 条/分；单私聊 1 条/秒 | **活动流逐条推送会撞限**（QQ 也有类似问题），需合并/限速 |
| 消息长度 | 4096 字符 | 终态结果需截断（dispatcher 的 `notify_text` 目前不截断） |
| 排版 | `MarkdownV2` / `HTML`；**不支持 `##` 标题**，不支持表格 | Ru41 输出规范是标准 Markdown（`##` 标题 + `**粗体**` + 列表 + LaTeX），需要一层 md→HTML 转换，失败降级纯文本 |
| 按钮 | Inline keyboard（`callback_data` ≤ 64 字节） | Ru41 的 `ReplyMarkup`（label + command 文本）可直接映射为 inline 按钮；但点击不产生新消息，而是 `callback_query`，需在 Telegram 侧把 callback 当作「代发命令」处理 |
| 媒体 | 官方 API 下载 ≤ 20MB、上传 ≤ 50MB | `send_media` 可实现；`MediaRef` 的 `ru41-media://N` 占位符机制需要单独适配（Telegram 没有等价内嵌引用） |
| 状态提醒 | BotFather 会监控「私聊回复率」，过低会发告警 | 必须对每条私聊消息及时回复，否则可能被降权 |

### 可选的高级能力（后续再说）

- `sendMessageDraft` / 流式草稿（Bot API 10.2+）：适合 Ru41 的活动流逐条推送。
- Ephemeral 消息（10.2+）：群里给单个用户发私密回复——正好对应 Ru41 的「群聊不展示私人内容」双闸。
- `setMyCommands`：把 Ru41 的命令表注册进 Telegram 命令菜单。

---

## 三、Ru41 现状地图（接入点盘点）

### 已有可复用的契约

| 契约 | 位置 | 说明 |
|---|---|---|
| `NotifyChannel` | `src/ports/channel.rs:7` | `send` / `send_media` / `send_with_markup`（后两个有默认降级实现） |
| `CommandHandler` / `CommandContext` | `crates/ru41-plugin-api/src/ports/commands.rs:116,151` | 渠道只负责收发，业务判断在插件；`ctx.reply_channel` 由适配器注入，**插件内不得手写渠道名** |
| `NotifyDispatcher` | `src/runtime/dispatcher.rs:15` | 按 `route.channel` 匹配 `channel_id()`，终态/进度/活动流/交互卡片统一派发 |
| 装配层 | `src/bootstrap/assembly.rs:30,75,138` | 渠道知识只允许出现在这里：`notify_channels` / `default_notify_route` / `default_notifier` |
| 渠道抽象目标 | `src/ports/channel.rs` | `target` 语义：`group:{openid}` / `user:{openid}`（QQ），Telegram 对应 `chat:{chat_id}` |

### 关键：插件层已经基本渠道无关

插件里的业务命令大多用 `ctx.reply_channel` 取渠道（如 `plugins/planner/src/qq_command.rs:192,228`、`plugins/lecture/src/handler.rs:256`），**没有**把 `"qq"` 写死。这意味着 Telegram 复用插件命令的成本很低——只要装配层把同一批 handler 注册给 Telegram 即可。

### 必须处理的硬编码耦合点

| # | 位置 | 问题 | 处理建议 |
|---|---|---|---|
| 1 | `crates/ru41-plugin-api/src/ports/commands.rs:203` | `msg_request_key()` 写死 `"qq:msg:"` | 改为带 channel 参数：`msg_request_key(channel, msg_id)`；或新增 `channel_msg_request_key` 并保留旧函数兼容 |
| 2 | `src/channels/qq/commands.rs:121,127,212` | 内置命令写死 `"qq"`（身份绑定判定、`notify.channel` 比对、会话 key） | Telegram 需要一份等价实现；**更好的做法**是把这块上提到 `ports/commands.rs` 做成渠道无关的公共命令模块，QQ/Telegram 共用 |
| 3 | `crates/ru41-plugin-api/src/module.rs:79` | `ModuleDef.qq_handlers` 字段名与 `QqHandlersFn` 类型名 | 新增 `channel_handlers`（或直接改名并保留 `qq_handlers` 兼容位），装配层按渠道分别注册 |
| 4 | `src/ingress/cli.rs:88,269` | CLI 只有 `Qq` 子命令 | 新增 `Telegram { Run, Send, Doctor, Onboard? }` |
| 5 | `src/ingress/cli_ops.rs:65` | 入口分发只认 qq | 新增 `telegram::run` 分支 |
| 6 | `src/ingress/cli_ops/migration.rs:538` | 渠道 → 环境变量映射表没有 telegram | 补 `channel.telegram.bot_token → RU41_TELEGRAM_BOT_TOKEN` |
| 7 | `src/ingress/cli_ops/status.rs:34` | 只报 `feishu_configured` | 新增 `telegram_configured` |
| 8 | `src/runtime/dispatcher.rs:465,468` | `effective_msg_id()` 的 240 秒窗口是 QQ 语义 | Telegram 走「永远保留 msg_id（作为 reply_to）」；建议按 `route.channel` 分支，而非全局行为 |
| 9 | `src/channels/qq/commands.rs:212` / `src/runtime/worker.rs:59` | 会话 key 形如 `qq:{target}` | worker 侧已用 `route.channel` 拼（渠道无关）；`/clear` 命令处需按渠道取 |
| 10 | `crates/ru41-plugin-api/src/ports/commands.rs:186` `require_private()` | 双闸已是渠道无关（`is_bound(reply_channel, ...)` + `is_c2c`） | Telegram 直接复用；但 Telegram 的「单聊」语义 = `chat.type == "private"`，映射到 `is_c2c` |

### 部署形态参考

- 生产：`192.168.1.223`（ssh 别名 `ru41-prod`），docker 容器 `ru41-*`，工作目录 `/home/windy2/ru41-mimo-smoke/`。
- 现有容器：`ru41-qq`、`ru41-mimo-worker`、`ru41-todo-remind`、`ru41-class-remind` 等 20+ 个。
- `ru41-qq` 启动命令：`--config /etc/ru41/ru41.toml qq run --backend pi`；**容器内无 `HTTP_PROXY` 环境变量**。
- `deploy/compose.yaml` 里只有 `browser` 服务注入了代理（`deploy/compose.yaml:316-320`）。

---

## 四、方案选型

### 方案 A：自研轻量适配器（推荐）

用现有 `reqwest` + `serde_json` 手写 Telegram REST 客户端，风格对齐 `src/channels/qq/api.rs`。

- ✅ 零新依赖，`cargo build` 体积与 CI 不变；与 `NotifyChannel` / `CommandHandler` 契约严丝合缝，不会出现两套抽象。
- ✅ 只实现需要的 6–8 个方法：`getUpdates` / `sendMessage` / `sendPhoto` / `sendDocument` / `answerCallbackQuery` / `setMyCommands` / `getMe`。
- ✅ 密钥可控：token 拼在 URL 里，必须确保不进日志（参考 `src/ingress/cli_ops/qq.rs:120` 的 `sanitize_error`）。
- ⚠️ 需要自己处理 Markdown→HTML 转换与消息分片（4096 限制）。

### 方案 B：用 teloxide 0.17.0

- ❌ 引入 dptree 依赖树 + 自己的 dispatcher 抽象，与 Ru41 的 `CommandHandler` 体系重复。
- ❌ teloxide 的 update 类型覆盖 Bot API 某个版本，Bot API 10.x 的新特性（ephemeral、widgets）要等上游更新。
- ⚠️ 最新版 2025-07-11 发布，下载量健康（近期 54 万/月），并非不可用——但对 Ru41 来说是「引入框架」而非「接一个渠道」，代价与收益不匹配。

**结论：方案 A。** Telegram Bot API 本质就是 JSON over HTTPS，框架化收益低于其带来的抽象冲突。

### 接收模式选型

| | `getUpdates` 长轮询 | `setWebhook` |
|---|---|---|
| 网络暴露 | 无，容器主动出网 | 需要公网 HTTPS 入口 + 反代 |
| 适配 Ru41 部署 | ✅ 直接复用 223 现有形态（无需新增 Caddy 端口/证书） | ⚠️ 与 Toonflow「一个应用一个 https 端口」策略冲突，要么走 238 的 Caddy 转发到 223，要么自签证书 |
| 延迟 | 长轮询 `timeout=30` 可做到准实时 | 最低延迟 |
| 多实例 | **不允许**并发轮询同一 token | 可多实例（按连接数限制） |
| 丢消息风险 | 24h 内未读会丢（服务重启期间）；需持久化 offset | Telegram 侧重试 |

**推荐：默认长轮询**，把 offset 持久化到 SQLite（`ru41` 已有 document 存储），避免重启后重复消费或漏消费；webhook 留作配置项。

### hermes-agent 对照评估（2026-11-15）

[NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent)（MIT，Python，26.3k star）是同类项目：个人 agent + **27 个消息平台**（telegram、qqbot、feishu、slack、discord、matrix、signal、whatsapp…）。它的 Telegram 接入在 `plugins/platforms/telegram/`（约 60 个模块，`adapter.py` 单文件 415KB）。拿它对照的目的：验证我们的方案有没有漏掉真实坑，以及哪些做法值得抄、哪些不该抄。

**它怎么做的**

| 方面 | hermes 的做法 | 对 Ru41 的意义 |
|---|---|---|
| 平台组织 | 每平台一个插件目录，统一 adapter 契约 + `gateway_env.py` + `plugin.yaml` 清单 | Ru41 现在是 `src/channels/` 写死 qq/feishu；短期不必重构，但**新渠道别再加深耦合** |
| 配置面 | `plugin.yaml` 声明式：`TELEGRAM_BOT_TOKEN`(password)、`TELEGRAM_ALLOWED_USERS`、`TELEGRAM_HOME_CHANNEL`、`TELEGRAM_WEBHOOK_SECRET` | 与我们的 `bot_token / default_chat_id / bind_secret` **一一对应，佐证配置面设计正确**；多出「用户白名单」这一维 |
| 准入 | 用户 ID 白名单 + `ALLOW_ALL_USERS` 开关 | Ru41 用「口令绑定 + 单聊双闸」（因为有待办/水电等私人数据）；**白名单可作第一层，不能替代双闸** |
| 收消息 | python-telegram-bot 长轮询 / webhook | 我们自研（Rust + reqwest，零新依赖），**接口能力对等** |
| 轮询存活 | `getWebhookInfo.pending_update_count` 探针：进程活着但 getUpdates 静默卡死也能发现；另有 409（同 token 双消费者）恢复阶梯 | **★ 值得抄**：我原方案没覆盖「轮询卡死但进程健康」 |
| 网络 | **DoH 解析 + `fallback_ips` 直连 IP 回退（保留 SNI）** + 显式代理 + TCP keepalive + 连接预算 | **★ 值得抄**：DoH/IP 回退正是我们 DNS 污染的解药，可作代理之外的兜底 |
| Markdown | 37KB 的 Markdown→MarkdownV2 转换器（强调配对、引用块、链接降级、裸 URL） | Ru41 输出规范窄得多（`#`/`**`/列表/代码块），**不需要这个规模** |
| 分片 | `split_formatted_mdv2`：按代码块/引用边界切，未闭合强调标记带到下一段，加 `(1/3)` 序号 | **★ 思路值得抄**：按裸字符切会破坏代码块 |
| 频控 | 洪泛窗口 + 草稿跳过 + 超预算延迟 | 需要；Ru41 先用简单令牌桶即可 |
| 空文本 | 不可见字符守卫（零宽、BOM、bidi 等） | Ru41 飞书/QQ 同样有空消息坑，**抄** |
| 平台提示 | 按平台往 system prompt 注入「你在 Telegram 上、## 会自动转换、不要用表格」 | Ru41 的 `src/markdown.rs` 应增渠道变体 |
| 入站细节 | contact 卡片、照片 burst 0.8s 去抖、album 1.5s、`/stop` 重置边界 | 与 Ru41 QQ 的「合并排水」同类，阶段 2 参照 |

**不该抄的**

- `python-telegram-bot` 依赖：Ru41 是 Rust，且已有 `NotifyChannel` / `CommandHandler` 契约，引入第二套抽象得不偿失（与上文结论一致）。
- 415KB 的 `adapter.py` 单体形态：抄结构不抄文件。
- 27 平台全量：Ru41 只需要 Telegram + 现有两个。
- MarkdownV2 全量语义：Ru41 输出规范窄得多。

**据评估对方案的修订（并入下面阶段 2 清单）**

1. 长轮询加 **`pending_update_count` 心跳探针**（轮询卡死自愈）+ **409 冲突**处理（同 token 双消费者）。
2. 网络层加 **DoH 解析 + 直连 IP 回退**作为代理失效兜底（代理本身会挂，兜底能救命）。
3. `src/markdown.rs` 增**渠道化平台提示**：Telegram 侧告诉模型「## 标题可用、不用表格、媒体用占位符」。
4. 配置面补**用户白名单**（`allowed_user_ids`）作为第一层准入；绑定双闸仍保留。
5. 分片按 **Markdown 结构边界**切（代码块/列表/引用），不按裸字符切。

---

## 五、分阶段实施计划

### 阶段 1：单向通知出口（Telegram 作为「第二个飞书」）

**目标**：`[notify] channel = "telegram"` 后，RSS 日报 / 论文日报 / 待办提醒 / 课程提醒都能推到 Telegram 私聊。

改动清单：

1. **新增** `src/channels/telegram/mod.rs`
   - `TelegramNotifier` 实现 `NotifyChannel`（`channel_id() == "telegram"`）
   - `send`: `POST /bot<token>/sendMessage`，body `{chat_id, text, parse_mode:"HTML", reply_parameters?}`
   - `send_with_markup`: `reply_markup.inline_keyboard`（`ReplyMarkup` → `[[{text, callback_data}]]`，`callback_data` 截断 64 字节）
   - `send_media`: `sendPhoto` / `sendDocument` / `sendVideo`，按 `MediaKind` 选方法，URL 直传或先 `getFile` 下载再上传
   - target 语义：`chat:{chat_id}`；`default` 占位符解析为配置的默认 chat（与飞书 `DEFAULT_TARGET` 同思路，避免 chat_id 落库）
2. **新增** `src/channels/telegram/markdown.rs`（或复用 `src/markdown.rs`）：Markdown → Telegram HTML
   - `## 标题` → `<b>标题</b>`；`**粗体**` → `<b>`；`` `code` `` → `<code>`；``` 代码块 ``` → `<pre>`；列表 `- ` → `• `
   - LaTeX `$...$` 保留原样（Telegram 无公式渲染，转成纯文本更稳）
   - **必须转义** HTML 特殊字符 `< > &`
   - 发送失败按「HTML → 纯文本」两级降级（对齐 `src/channels/qq/api.rs:487` 的降级思路）
3. **改** `crates/ru41-plugin-api/src/config.rs:278`：加 `TelegramSection { bot_token, default_chat_id, bind_secret? }`
4. **改** `src/bootstrap/assembly.rs`
   - `notify_channels()`：有 token 即注册 Telegram
   - `default_notify_route()`：`"telegram" => NotifyRoute { channel:"telegram", target:"default", msg_id:None }`
   - `default_notifier()`：`"telegram" => TelegramNotifier`
   - 密钥读取：`RU41_TELEGRAM_BOT_TOKEN` > `config.channel.telegram.bot_token`
5. **改** `src/ingress/cli_ops/status.rs:34`：新增 `telegram_configured`
6. **改** `src/ingress/cli_ops/migration.rs:538`：补环境变量映射
7. **改** `ru41.example.toml:70` 附近：加 `[channel.telegram]` 示例（token 留空、注释说明用环境变量）
8. **改** `src/main.rs:274` 的错误提示（"no notify channel configured (e.g. channel.feishu.webhook_url)"）→ 补 telegram 示例
9. **测试** `tests/telegram_notify.rs`：照 `tests/feishu_notify.rs` 的 mock HTTP server 写法，覆盖发送成功 / 平台拒绝 / 目标非法 / token 脱敏

**风险**：低。可独立上线，出问题只需把 `[notify] channel` 切回 `qq`。

### 阶段 2：双向消息网关（Telegram 作为「第二个 QQ」）

**目标**：私聊里发自然语言 → 入队给 Agent → 结果回私聊；命令、待办、日程、身份绑定、活动流全部可用。

改动清单（照 `src/channels/qq/` 结构）：

1. **新增** `src/channels/telegram/api.rs`：REST 客户端 + `getUpdates` 轮询 + `getMe` 自检
2. **新增** `src/channels/telegram/mod.rs`：`TelegramBot`（收消息幂等入队、命令执行、扩展命令注册、合并排水、open interaction 答复）
3. **新增** `src/channels/telegram/commands.rs`：内置命令（Telegram 侧命令词必须用 ASCII，Telegram 命令名只允许拉丁字母数字下划线，`/任务`、`/日程` 这类中文命令**不可用**，需设计 ASCII 别名：`/tasks` `/todo` `/schedule` `/bind` `/cancel` `/usage` `/help` `/new`）
4. **改** `crates/ru41-plugin-api/src/ports/commands.rs:203`：`msg_request_key` 渠道化
5. **改** `crates/ru41-plugin-api/src/module.rs:79`：`ModuleDef` 增加渠道化 handler 注册（建议 `channel_handlers: HashMap<&str, HandlersFn>` 或新增 `telegram_handlers`）
6. **改** `src/bootstrap/assembly.rs:47`：`qq_command_handlers` → 渠道化版本，QQ 与 Telegram 各注册一份
7. **改** `src/ingress/cli.rs`：`Telegram { Run, Send, Doctor }`；`src/ingress/cli_ops.rs:65` 加分支；新增 `src/ingress/cli_ops/telegram.rs`
8. **改** `src/runtime/dispatcher.rs:465`：被动窗口按渠道分支（Telegram 永久保留 msg_id）
9. **改** `src/channels/qq/commands.rs` / 新增公共命令模块：把「任务列表 / 任务详情 / 取消 / 绑定 / 用量 / 新建」上提到 `ports::commands` 做成渠道无关（避免两套命令实现漂移）
10. **新增** offset 持久化：把 `update_id` 存到 document（`kind = "telegram_offset"`），重启后从上次 offset 继续
11. **新增** 轮询存活探针：定期读 `getWebhookInfo.pending_update_count`，卡死则重启轮询；并处理 409（同 token 被两个消费者轮询）
12. **新增** 网络兜底：DoH 解析 + 直连 IP 回退（保留 SNI），代理失效时不至于完全哑火
13. **改** `src/markdown.rs`：增渠道化平台提示（Telegram：## 标题可用、不用表格、媒体占位符）
14. **新增** 用户白名单配置（`allowed_user_ids`）作第一层准入；口令绑定双闸保留
15. **改** 消息分片：按 Markdown 结构边界（代码块/列表/引用）切，不按裸字符切
16. **测试** `tests/telegram_gateway.rs` + `tests/telegram_api.rs`

> 11–15 项来自上文 hermes-agent 对照评估。

### 阶段 3：交互增强（可选）

- `callback_query` → 把 Ru41 的 inline 按钮「代发命令」映射为回调，直接执行（省掉一次用户点击后的文本往返）
- `sendMessageDraft` 流式草稿 → 活动流逐条推送不再刷屏
- Ephemeral 消息 → 群内给单个绑定用户发私人内容（待办、水电）
- `setMyCommands` → 命令菜单注册

---

## 六、配置与密钥

```toml
[channel.telegram]
# bot token 含密钥，建议只用环境变量注入：
#   RU41_TELEGRAM_BOT_TOKEN
# bot_token = ""
# 默认通知目标（私聊 chat_id），缺省时用 "default" 占位符 + 配置解析
# default_chat_id = ""
# 身份绑定口令（未配置时 /bind 关闭）
# bind_secret = ""
```

**密钥纪律**：
- token 出现在 URL path 里，**任何日志/错误信息/`config show`/`dump` 都不得出现完整 URL**。参考 `src/ingress/cli_ops/qq.rs:120` 的 `sanitize_error` 做法，统一做 `token → <redacted>` 替换。
- `ru41.toml` 已被 gitignore；compose 用环境变量注入（`deploy/roll-set-env.sh` 已有 `RU41_*` 注入范式）。
- BotFather 的 `/token` 可随时轮换；轮换后改环境变量重启容器即可。

---

## 七、部署（生产机 223，实测数据）

### mihomo 现状与规则归属（2026-11-15 实测）

mihomo 是**健康运行**的容器（`metacubex/mihomo:latest`，`network_mode: host`，配置 `/home/windy2/mihomo/config.yaml`），而且**生产机确实就在规则代理里**。

代理组（mihomo 控制接口 `127.0.0.1:9090/proxies` 运行时状态）：

| 组 | 类型 | 当前选中 |
|---|---|---|
| `PROXY` | fallback | `🇺🇸 美国专用` |
| `🇺🇸 美国专用` | fallback | `🇺🇸 美国 MT1-MUX` |
| `🇭🇰 香港专用` | fallback | `🇭🇰 香港 MT1` |
| `GLOBAL` | selector（内置） | `DIRECT` |

（配置里只有这 3 个自建组；日志里出现过的 `🇸🇬 新加坡专用` 属于旧配置版本，现配置已无。）

关键规则（`config.yaml` 第 471–478 行）：

```
- SRC-IP-CIDR,192.168.1.244/32,🇺🇸 美国专用,no-resolve
- SRC-IP-CIDR,127.0.0.0/8,🇺🇸 美国专用,no-resolve      # 宿主自身经代理端口出网
- SRC-IP-CIDR,192.168.1.223/32,🇺🇸 美国专用,no-resolve # ← 生产机本体
- SRC-IP-CIDR,192.168.1.218/32,🇭🇰 香港专用,no-resolve # 本工作站
- SRC-IP-CIDR,192.168.1.247/32,🇺🇸 美国专用,no-resolve
- SRC-IP-CIDR,172.17.0.0/16,🇺🇸 美国专用,no-resolve     # docker0 网段
...
- MATCH,PROXY                                           # 兜底
```

→ **生产机 `192.168.1.223` 归属 `🇺🇸 美国专用`（→ 美国 MT1-MUX 节点）；docker 容器网段同样归美国组。**

我实测的两条 Telegram 请求在 mihomo 日志里明确落到这个组：

```
[TCP] 127.0.0.1:50464  --> api.telegram.org:443  match SrcIPCIDR(127.0.0.0/8) using 🇺🇸 美国专用[🇺🇸 美国 MT1-MUX]
[TCP] 172.20.0.29:57850 --> api.telegram.org:443 match Match using PROXY[🇺🇸 美国 MT1-MUX]
```

**真正的卡点不是「没有规则代理」，而是「流量没进 mihomo」。** 两个原因：

1. **配置里根本没有 `tun:` 段**（`grep '^tun:'` 无命中）→ 从未启用透明代理，mihomo 只对**显式指向 7890/7891/7892/7893 的客户端**生效。
   （更正：本文早前版本把 `dns:` 段的 `enable: true` 误读成了 `tun.enable: true`，实际不存在 tun 配置。）
2. **系统 DNS 被污染**：宿主 DNS 是路由器 `192.168.1.1`，不是 mihomo 的 1053；`api.telegram.org` 实测解析到 Facebook IPv6 `2a03:2880:f11c:8183:face:b00c:0:25de`、假 IPv4 `128.242.245.180`、`59.24.3.173`；容器内（`ru41-qq`）同样是污染结果。而 mihomo 自己的 DNS 是干净的：`dig @127.0.0.1 -p 1053 api.telegram.org` → `198.18.11.153`（fake-ip 段）。

宿主命令行没有任何代理变量（`/etc/environment`、`~/.profile`、`~/.bashrc` 均无 `http_proxy`），现有 ru41 容器也都没注入——只有 `browser` 服务有。

实测对照：

```bash
# 宿主直连：解析到污染 IP，连接卡死直到超时
$ ssh ru41-prod 'curl -sv --max-time 8 https://api.telegram.org/bot0/getMe'
* Host api.telegram.org:443 was resolved.
*   Trying [2a03:2880:f11c:8183:face:b00c:0:25de]:443...
*   Trying 128.242.245.180:443...          # 卡住 → 超时

# 宿主显式走 mihomo：mihomo 侧解析并按规则出站，正常
$ ssh ru41-prod 'curl -x http://127.0.0.1:7890 https://api.telegram.org/bot0/getMe'
tg-proxy:404 t=1.27s                       # 404 = token 无效的业务正常响应

# 最贴近最终部署形态：ru41-net 里的容器 + 代理环境变量
$ docker run --rm --network ru41-net -e HTTPS_PROXY=http://172.17.0.1:7890 \
    curlimages/curl:latest -s -o /dev/null -w "%{http_code} t=%{time_total}\n" \
    https://api.telegram.org/bot0/getMe
404 t=1.20s

# 响应体确认是 Telegram 真服务器的业务响应（排除中间层拦截/伪造）
$ docker run --rm --network ru41-net -e HTTPS_PROXY=http://172.17.0.1:7890 \
    curlimages/curl:latest -sS https://api.telegram.org/bot123456:TEST/getMe
{"ok":false,"error_code":401,"description":"Unauthorized"}

# POST + JSON 同样通（sendMessage 走的就是这条路径）
$ docker run --rm --network ru41-net -e HTTPS_PROXY=http://172.17.0.1:7890 \
    curlimages/curl:latest -sS -X POST -H "Content-Type: application/json" \
    -d '{"chat_id":1,"text":"ping"}' https://api.telegram.org/bot123456:TEST/sendMessage
{"ok":false,"error_code":401,"description":"Unauthorized"}

# 容器出站 IP：64.118.155.129（美国节点，印证命中 🇺🇸 美国专用）
```

→ **结论：Telegram 容器只需显式注入 `HTTP_PROXY/HTTPS_PROXY=http://172.17.0.1:7890`**，
剩下的分组、选节点、故障切换全部由 mihomo 现成的规则代理接管（走 `MATCH → PROXY → 🇺🇸 美国专用 → 美国 MT1-MUX`）。
这与仓库里 `deploy/compose.yaml` 的 `browser` 服务（`deploy/compose.yaml:316-320`）完全同款，是这台机器上**已被验证过的出网模式**。

另外提醒：即使把容器 DNS 指到 mihomo 的 1053 也**不够**——DNS 污染只是表象，Telegram 的目标段在大陆同样被墙，必须让流量真正从 mihomo 出站。

### 关于「要不要顺手把透明代理补上」

现状是**配置里没有 tun 段**，若想开启需新增 `tun:` 段并给 mihomo 容器映射 `/dev/net/tun`（或 `privileged: true`），属于系统级改动：收益对 Telegram 接入为零（显式代理已足够，且分组更可控），却会改变整机流量路径、影响面大。按既有约定，系统级改动需先调研、给用户过目、附回滚方式再动手——建议单开一轮，不和本次接入混在一起。

### 新增 compose 服务（草案）

```yaml
  telegram:
    <<: *ru41-common
    profiles: [telegram]
    volumes: *ru41-volumes
    environment:
      # Telegram 在中国大陆不可直连，必须走宿主机 mihomo（docker0 网关）
      HTTP_PROXY: http://172.17.0.1:7890
      HTTPS_PROXY: http://172.17.0.1:7890
      NO_PROXY: localhost,127.0.0.1
    command: ["--config", "/etc/ru41/ru41.toml", "--db", "/data/state.db", "telegram", "run"]
```

启动：`docker compose -f deploy/compose.yaml --profile telegram up -d telegram`（或沿用 223 上现行的 `docker run` 式运维）。

### 部署注意

- **不要**用公网 webhook 方案作为第一版：会把公网入口引入家庭局域网服务，收益不抵风险。长轮询足够。
- 测试用**独立 bot**（BotFather `/newbot`），官方建议另建测试 bot 避免影响正式用户；`file_id` 与 bot 绑定，不能跨 bot 复用。
- 同一个 token 只能有一个实例轮询；223 上已有 `ru41-qq` 等容器，Telegram 容器需独占自己的 token。

---

## 八、风险清单

| 风险 | 等级 | 应对 |
|---|---|---|
| 生产机直连不通 | **高（已发生）** | 根因是 mihomo 只在显式代理模式生效（TUN 未起 + DNS 污染），不是代理缺失。Telegram 容器必须注入 `HTTP_PROXY/HTTPS_PROXY=http://172.17.0.1:7890`；`ru41 telegram doctor` 里加一次 `getMe` 连通性自检，失败直接报「代理未生效」 |
| 群里收不到自然语言（隐私模式） | 中 | 明确产品口径：Telegram 群里走 `/命令`；私聊自然语言不受限；不关闭隐私模式 |
| 单实例约束 | 中 | 部署规范写死「一个 token 一个实例」；doctor 检查 `getUpdates` 冲突错误 |
| 频控（单私聊 1 条/秒） | 中 | 活动流合并（≥2s 批量）、终态结果单独发；429/400 retry_after 退避 |
| Markdown 渲染差异 | 中 | md→HTML 转换 + 纯文本降级；发送前预校验 |
| token 泄露 | 中 | 脱敏工具函数 + 测试断言（照 `tests/cli_ops.rs` 的 secret 断言写法） |
| 消息 4096 截断 | 低 | 发送前按字符截断并附「详情见任务记录」 |
| BotFather 状态提醒（回复率过低） | 低 | 每条私聊都回；命令也回 |
| 插件命令注册方式偏 QQ | 中 | 阶段 2 前先做 `ModuleDef` 渠道化重构，避免 Telegram 只能拿到一半插件命令 |

---

## 九、回滚

本方案是**纯增量**，回滚成本低：

1. **阶段 1 回滚**（最常用）：把 `[notify] channel` 改回 `qq`/`feishu` → Telegram 立刻停止接收（容器不启动即可）。
2. **代码回滚**：`src/channels/telegram/` 整目录删除 + 4 处接线（`assembly.rs` / `config.rs` / `status.rs` / `migration.rs`）还原 → `git checkout` 即可，无数据迁移、无 schema 变更。
3. **容器回滚**：`docker compose --profile telegram stop/rm telegram`；不改其他容器。
4. **密钥回滚**：`docker compose ... down` 后删除 `RU41_TELEGRAM_BOT_TOKEN` 环境变量；BotFather `/token` 可随时轮换。
5. **数据**：阶段 2 若引入 offset 持久化（document kind `telegram_offset`），删除该 kind 的文档即可，不影响其他数据。

---

## 十、验收标准

**阶段 1**
- `ru41 telegram doctor` 输出 `getMe` 成功 + bot 用户名，且输出中不含 token。
- `[notify] channel = "telegram"` 后，`ru41 task add --key t:1 --prompt "hi" --notify` 能在 Telegram 私聊收到结果。
- 平台拒绝时退避重试（dispatcher 的 delivery 记录），不丢任务。
- `cargo test --locked` 全绿；新增测试覆盖 token 脱敏。

**阶段 2**
- Telegram 私聊发自然语言 → 3 秒内出现「已入队」→ 终态结果回到同一会话。
- `/tasks`、`/todo`、`/bind`、`/usage` 等命令可用。
- 群内私域数据（待办、水电）在未绑定或群聊场景下被拒绝（复用 `require_private` 双闸）。
- 重启容器后不重复消费历史消息、不丢未处理消息（offset 持久化生效）。

---

## 参考资料

- [Telegram Bot API（官方，当前 10.4）](https://core.telegram.org/bots/api)
- [Bot Features（命令/键盘/隐私模式/BotFather/本地 Bot API）](https://core.telegram.org/bots/features)
- [Bot API 变更日志](https://core.telegram.org/bots/api-changelog)
- [telegram-bot-api（自建 Bot API server 源码）](https://github.com/tdlib/telegram-bot-api)
- [teloxide（Rust 框架，参考未采用）](https://github.com/teloxide/teloxide)
- crates.io API 实测：`teloxide` 最新 0.17.0（2025-07-11，近期下载 54 万/月）；`rust-telegram-bot` 最新 0.0.1（2023-09-20，已停更）
- 本仓库参考实现：`src/channels/qq/`（全功能网关）、`src/channels/feishu.rs`（单向通知）、`src/ports/channel.rs`（渠道契约）、`src/bootstrap/assembly.rs`（装配层）
