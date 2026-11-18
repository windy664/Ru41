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

### 「哪些必须报」要写进 interests，不要靠每天现判

`pool_keyword_filter = false`（默认）= **全量入池**，哪些条目值得报由 digest 里的
agent 每次**现场判断**。这条链路上：

- 条目**采集到了、也进了 digest prompt**，不代表会被写进日报；
- agent 会自己发明「舍弃纪律」，措辞每次都不一样。

2026-11-18 实测：Java4Shell（OpenJDK Base64 解码器 RCE，CVSS 9.2，无需认证即可
远程接管）在早报里被写进了「已舍弃：…安全漏洞细节…」那一行——因为 `interests`
里没提安全，prompt 也没有相关纪律。

改法是**改配置**（`interests` 就是「用户推荐参数」的位置）：把必报类别写进去，例如

> …；突发高危安全漏洞（0day / RCE / 无需认证即可远程接管 / 已在野利用的 CVE）
> 必须单独报出来，不要当噪声舍弃

改完用同一批候选重跑一次 digest 即可验证（见 `ru41 task add --key <新 key> --prompt <旧 prompt> --notify`）。
反过来，**要舍弃**的类别也该落成 `drop_title_patterns`（入池前丢，比在 digest 里拦省 LLM 调用），
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
