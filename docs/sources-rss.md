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
| **出口** | 命中的条目**单独成一条 digest 任务**（`rss:digest:must:{slot}:{date}`），prompt 里只有这些条目、明令「一条都不许舍弃」；普通日报里不再包含它们 |

关键差别：必报不再依赖 LLM 的判断力 —— 那条任务里**只有**必报条目，没有别的条目跟它
抢注意力，prompt 也明确禁掉了「舍弃纪律」。必报任务先入队 → 告警先到。

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
```

`days=` 是各源自己的时间窗（GHSA/KEV 默认 7 天，NVD 默认 2 天）：KEV 是**累积目录**
（全库 1700+ 条），不设窗口就会每天把全库重报一遍。

**要舍弃**的类别仍该落成 `drop_title_patterns`（入池前丢，比在 digest 里拦省 LLM 调用），
别让 agent 每天重新判一遍。

## 运行

```bash
cargo run --locked -- rss poll   # 单轮（适合系统 cron）
cargo run --locked -- rss run    # 常驻周期轮询
```

## 边界

- 支持 RSS 2.0 与 Atom 两种格式（`parse_feed` 自动识别；Atom 的链接取 rel=alternate，published 优先于 updated）。
- 全文不抓取，摘要注明材料范围由 prompt 约束；X 情报等需登录态的来源不在此模块。
- 不写 Vault 来源笔记（等 P1 Obsidian 闭环）。
