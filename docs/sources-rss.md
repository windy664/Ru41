---
type: source-adapter
status: implemented
source: RSS 2.0 订阅源
updated: 2026-10-14
tags: [Ru41, RSS, intelligence, source]
---

# RSS 信息流追踪说明

通用资讯来源模块：定时抓取配置的订阅源（RSS 2.0 / Atom），去重与关键词粗筛后合成精选 digest 任务交 Codex，终态经 `[notify]` 默认出口推送。与 [arXiv 追踪](sources-arxiv.md) 共用同一 digest 管线；代码：`src/adapters/sources/feed.rs`（通用 feed 抓取与双格式解析）、`src/app/digest.rs`（去重/过滤/入队管线）、`src/modules/rss/service.rs`（逐源编排与 prompt 模板）。

## 与论文模块的差异

- 输出口径按设计文档 3.4 四段式：发生了什么 → 为什么重要 → 与我有关之处 → 下一步行动，并要求区分事实与推断。
- 去重键为 `{feed_url}#{guid}`（guid 缺省退回 link）：同一 guid 在不同源下不互相影响；同一事件被多源报道的聚合留给 Agent 在 digest 内判断。
- 单源抓取失败不中断其他源，错误记入当轮报告的 `errors`。
- description 中的 HTML 会被剥离、实体还原并截断到 2000 字符。

## 配置（ru41.toml）

```toml
[notify]
channel = "feishu"

[sources.rss]
feeds = [
  "https://www.jiqizhixin.com/rss",
  "https://www.qbitai.com/feed",
]
keywords = ["agent", "llm", "安全"]   # 可选粗筛；缺省不过滤
interests = "AI 安全方向本科生"        # 注入 digest prompt
max_items_per_feed = 50
interval_secs = 21600                 # rss run 轮询间隔，默认 6 小时
notify = true
```

### 「哪些必须报」现在是**代码硬闸**，不再靠 interests

> 2026-11-22 起，这条从「写在 interests 里求 LLM 照做」改成**代码判定**。

原来的做法是把必报类别写进 `interests`（"突发高危安全漏洞（0day / RCE / 无需认证即可
远程接管 / 已在野利用的 CVE）必须单独报出来"），靠 prompt 求 LLM 照做。实测不牢：

- 2026-11-18：Java4Shell（OpenJDK Base64 解码器 RCE，CVSS 9.2，无需认证即可远程接管）
  被写进了「已舍弃：…安全漏洞细节…」那一行；
- 2026-11-21：同期池里 OpenSSL CVSS 9.8 / 群晖 DSM 9.8 / Ruby RCE / PHP 8.6 全被丢掉。

根因有两条，都不是「模型不听话」：

1. **判据不在代码里** —— prompt 只说「筛选值得关注的」，哪些必须报每天由 LLM 现场判；
2. **料根本进不来** —— 订阅的全是资讯站（机器之心/IT之家/…），漏洞公告要么不进这些源，
   要么是二手转述、没有 CVSS 与编号。「PHP 8.6 修了 6 个 Critical RCE」这类信息**压根没进池**。

现在的做法（`plugins/rss/src/security.rs`）：

| 层 | 做什么 |
|---|---|
| **源** | 新增 4 个权威漏洞源（见下表），条目带 GHSA/CVE 编号、CVSS 分值与向量、官方描述 |
| **判据** | 代码层判定：CVSS ≥ 9.0 / Critical / RCE / 0day / 无需认证即可利用 / 已在野利用 → 强信号；只有 CVE 编号 → 仅当条目来自权威源时算必报 |
| **空洞** | 没有编号、没有评分、没有版本号、也读不出漏洞语境的条目（典型「XX 发布安全公告」）**不成条** —— 报了只是占位 |
| **出口** | 命中的条目**并进当天日报的第一部分**（`🚨 必报 N 条` 开头），每条三行、明令「一条都不许舍弃」；普通资讯是第二部分 |

关键差别：必报不再依赖 LLM 的判断力 —— 必报条目在正文**最前面**、有独立的纪律段
（「一条都不能少」，空洞也要列），资讯段的「舍弃纪律」**不作用于**它。

> **2026-11-25 改动**：以前必报是**单独成一条 digest 任务**（`rss:digest:must:{slot}:{date}`），
> 跟普通日报分成两条消息发，用户嫌「日报和漏洞怎么分开单独发」。
> 现在合成一条（`merge_must_report`，缺省 true）。
> ⚠️ 合成时**纪律必须分区** —— 直接把两套材料混在一起的话，普通日报 prompt 里那句
> 「舍弃纪律」会把漏洞一起舍掉（Java4Shell 就是这么丢的）。
> ⚠️ 必报段必须写在**最前面**：dispatcher 的空报判定只看正文前 80 字，
> `📭` 开头会被当空报静默吞掉。
> 要回到「分开两条」的老路：`merge_must_report = false`。

### 权威漏洞源

| 前缀 | 源 | 带什么 | 备注 |
|---|---|---|---|
| `ghsa+` | GitHub Advisory Database（`api.github.com/advisories`，免认证） | GHSA 编号、CVE 编号、CVSS 分值与向量、官方描述、严重等级 | 结构化漏洞源里最干净的一个 |
| `kev+` | CISA 已知被利用漏洞目录（JSON） | CVE 编号、厂商/产品、处置期限(dueDate)、勒索软件标记 | **已在野利用**的那一档：现在就修 |
| `nvd+` | NVD CVE API 2.0 | CVSS v3.1/v4.0 评分与向量、描述 | 只取 CRITICAL（全量每天上千条，会淹掉日报） |
| `php+` | php.net 发布 Atom + ChangeLog | 版本号、GHSA-/CVE- 编号、修复描述、受影响版本提示 | Atom 条目本身是空洞公告（「有新版本了」），真正的安全修复在 ChangeLog 里 |

配置样例（`[sources.rss].feeds`）：

```toml
feeds = [
  # …原有资讯源…
  "ghsa+https://api.github.com/advisories?per_page=100&sort=published&direction=desc&days=7",
  "kev+https://www.cisa.gov/sites/default/files/feeds/known_exploited_vulnerabilities.json?days=7",
  "nvd+https://services.nvd.nist.gov/rest/json/cves/2.0?days=2",
  "php+https://www.php.net/releases/feed.php",
]
must_report = true   # 缺省 true
merge_must_report = true   # 缺省 true：必报并进日报（合成一条）；false = 分开两条
```

`days=` 是各源自己的时间窗（GHSA/KEV 默认 7 天，NVD 默认 2 天）：KEV 是**累积目录**
（全库 1700+ 条），不设窗口就会每天把全库重报一遍。

**要舍弃**的类别仍该落成 `drop_title_patterns`（入池前丢，比在 digest 里拦省 LLM 调用），
别让 agent 每天重新判一遍。生产上已加：黑五 / 黑色星期五 / 礼品卡 / 优惠券 / 秒杀 / 游戏充值
（纯购物促销；**免费/限量的 API 额度、云服务试用、开源模型发布、学生优惠不算促销**，
那属于关注方向，交给 agent 判断）。

## ⚠️ 候选集的两道截断（2026-11-27 实测，连续 6 期的回归）

日报里「有奖金的比赛/黑客松」「优质实习」两个方向长期为 0，不是 agent 不选，
是**候选在进材料前就被砍了**。

| 日报 | 候选 | 进了材料 | 机会类进了几条 |
| :--- | :--- | :--- | :--- |
| 11-24 早 | 80 | 80 | ✅ 黑客松×2 实习×3 奖金 |
| 11-25 早 | 53 | 53 | ✅ 黑客松×3 实习×12 奖金×5 |
| 11-25 晚 | 80 | 56 | ❌ 零 |
| 11-26 早 | 80 | 55 | ✅ 实习×16 招聘×6 |
| 11-26 晚 | 80 | 27 | ❌ 零 |
| 11-27 早 | 76 | 26 | ❌ 零（5 个有奖金的黑客松 + 1 条实习一条没进） |

**两道截断都按「采集时间」淘汰**，而机会类条目（有截止日期的比赛/黑客松/实习）
**发布一次、留在池里直到截止日** ⇒ 天然排在池子最前部 ⇒ 被系统性淘汰：

| 截断 | 位置 | 原来的行为 |
| :--- | :--- | :--- |
| A | 候选池封顶 80 条 | `pool.drain(..len-80)` ⇒ 丢最旧的（机会类就在最前面） |
| B | 提示词预算 | 逐条填、装不下就丢；**预算按字节算**（中文 1 字 3 字节）⇒ 26,000 字节只放得下 ~24 条 |

### 现在的做法

1. **机会源优先**（`opportunity_id_prefixes`）：池子封顶时**不淘汰**它们；
   组装材料时**排最前**（预算不够时丢的是别的条目）。
   缺省覆盖 wqai / devpost / lablab / df / xfyun / hackertrip / hack123 /
   gsoc / lfx / hackathoncom / mianling（与各适配器的 id 前缀一致）。
2. **预算均分**（`build_material`）：给每条同一个字节预算、摘要按预算尽量给长
   （`truncate_bytes` 按字节截，不切坏 UTF-8）⇒ 76 条候选能全进材料，
   摘要略短（~200 中文字）而不是丢掉 52 条。
3. **摘要上限 400 → 250 字**：每条从 ~1,068 字节降到 ~700 字节，
   80 条约 54 KB，加指令段与必报段仍在入队上限（**65,536 字节**）内。
4. **头部计数与实际一致**：「候选 76 条，按预算带了 N 条」，丢了多少也写出来。
   原来头部写 76、正文只有 24 —— 模型和读者都不知道漏了什么。
5. **纪律**：加「促销纪律」（纯购物促销舍弃；但免费/限量 API 额度、云服务试用、
   开源模型发布、学生优惠**属于关注方向**）与「条目纪律」（每条必须是具体资讯，
   不许写「本条合并说明」这类占位条目 —— 2026-11-27 那期出现过一条占坑）。

### 顺带一提：`keywords` 在 daily 模式下**不起作用**

`pool_keyword_filter = false`（生产配置）⇒ 全量入池，关键词粗筛被跳过
（代码注释说这是故意的：粗筛容易误杀标题不含关键词但实质相关的条目，尤其英文源）。
⇒ 配置里那 20 多个词目前是**摆设**，留着会让人以为它在筛。

## 运行

```bash
cargo run --locked -- rss poll   # 单轮（适合系统 cron）
cargo run --locked -- rss run    # 常驻周期轮询
```

## 边界

- 支持 RSS 2.0 与 Atom 两种格式（`parse_feed` 自动识别；Atom 的链接取 rel=alternate，published 优先于 updated）。
- 全文不抓取，摘要注明材料范围由 prompt 约束；X 情报等需登录态的来源不在此模块。
- 不写 Vault 来源笔记（等 P1 Obsidian 闭环）。
