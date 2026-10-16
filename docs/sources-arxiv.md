---
type: source-adapter
status: implemented
source: arXiv 公共 API
updated: 2026-10-14
tags: [Ru41, arXiv, papers, source]
---

# arXiv 论文追踪说明

第一个信息来源模块：定时抓取 arXiv 新论文，经去重与关键词粗筛后合成**一份每日精选 digest 任务**交给 Codex 分析，终态经通知渠道推送。代码：`src/adapters/sources/arxiv.rs`（协议与 Atom 解析）、`src/app/digest.rs`（与 RSS 共用的去重/过滤/入队管线）、`src/app/papers.rs`（编排与 prompt 模板）。

## 形态：digest 而非逐篇推送

每次轮询的候选论文合并为一个分析任务（对应设计文档「每日精选 / 合并简报」），Codex 一次调用完成筛选与结构化摘要，只推送一条消息。逐篇深入分析后续以单独任务触发。

## 抓取约定

- 走官方 API `https://export.arxiv.org/api/query`（免 key），查询 `cat:` 分类按提交时间倒序。
- 官方限速约 1 次/3 秒；本场景每天个位数请求，客户端带标识性 User-Agent，遵守 arXiv 对自动化客户端的要求。
- 只取摘要级元数据；Agent 输出要求标注「基于摘要」，不补写摘要外的结论。

## 去重语义

- 规范 arXiv ID（去版本号）作 records 键（kind `arxiv_seen`），抓取成功即登记，与 digest 任务成败解耦——任务失败由任务系统重试，不靠重复抓取。
- 已见 ID 的新版本不重复入队；候选为空时不创建任务。
- digest 任务幂等键：`arxiv:digest:{日期}:{首篇id}-{篇数}`。

## 配置（ru41.toml）

```toml
[notify]
channel = "feishu"      # 全局默认通知出口；渠道选择只在这里出现

[sources.arxiv]
categories = ["cs.CR", "cs.AI", "cs.CL"]      # 必填，追踪的分类
keywords = ["prompt injection", "jailbreak"]  # 可选，标题/摘要粗筛；缺省不过滤
interests = "AI 安全方向本科生，关注 LLM/Agent 安全"  # 注入 digest prompt 供相关性判断
max_results = 100          # 单次拉取上限，默认 100
interval_secs = 21600      # papers run 的轮询间隔，默认 6 小时
backend = "codex"          # digest 任务底座
notify = true              # 终态后经默认出口推送，默认 true
```

业务配置不点名渠道：digest 任务只说「要通知」，由 `[notify] channel` 统一决定出口——飞书投递到配置的 webhook，QQ 投递到 `channel.qq.default_notify_target`（主动推送受平台配额限制）。渠道名 → 路由的转换只在装配层（`bootstrap/assembly.rs`）发生。

## 运行

```bash
cargo run --locked -- papers poll            # 单轮（适合系统 cron）
cargo run --locked -- papers poll            # 配了 [notify] 则终态自动推送
cargo run --locked -- papers run             # 常驻周期轮询
```

digest 任务由独立的 `worker` 执行，通知由 `notify`（或 `qq run` 内派发循环）发出——三个进程各司其职，均可在无公网环境运行（全出站连接）。

## 边界

- 精读已支持：QQ 发「精读 <arxiv-id>」按 ID 取回论文并入队精读任务（`fetch_by_id` + `deep_read_task`，见 docs/channels-qq.md）。
- 不写 Vault 来源笔记：属 P1 Obsidian 闭环，启动后 digest 中的论文再落成来源笔记。
- 暂未接 OpenAlex/Semantic Scholar 补全（引用数、TLDR、DOI 关联）；关键词粗筛噪音过大时再接入。
- 不下载全文；会议追踪（OpenReview）与 RSS 通用源是后续独立来源模块。
