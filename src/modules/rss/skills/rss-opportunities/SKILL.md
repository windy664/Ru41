---
name: rss-opportunities
description: 查询机会雷达的历史日报与最新机会（免费 AI 额度、竞赛黑客松、实习招聘、顶会动态）。当用户问「最近有什么机会」「今天的雷达」「有什么免费 token/比赛/实习信息」时使用。日报存在任务数据库，用 ru41 task 命令查。
---

# 机会雷达查询

## 数据在哪

- 日报是任务数据库里的 digest 任务（request_key 形如 `rss:digest:morning:2026-10-25` / `rss:digest:evening:...`），每天最多两份（08:00 早报、21:00 晚报），QQ 会推送。
- 查询方法：
  1. `ru41 --config /etc/ru41/ru41.toml --db /data/state.db task list --limit 30` 找到 key 以 `rss:digest:` 开头的任务 id
  2. `ru41 --config /etc/ru41/ru41.toml --db /data/state.db task show <id>` 读日报正文
- 不要找 vault 里的日报文件——日报不落 Obsidian（05-Opportunities 只放真正值得行动的机会笔记）。

## 雷达在盯哪些源

- RSS 信息流：HackerNews、HuggingFace Blog、V2EX、ProductHunt、CSDN 极客头条、IT之家、极客公园、GitHub Blog·hackathon、Supabase Blog、r/LocalLLaMA
- 赛事平台（伪 feed 解析）：Devpost（海外黑客松）、lablab.ai（纯 AI 黑客松）、DataFountain（国内 CCF 系数据赛）、HackerTrip（中文黑客松聚合，公开 JSON API）、wqai 湾区 AI 日历（赛事表格 + 官方公共 ICS：大湾区六城线下活动/峰会/沙龙，含城市场馆票务）、讯飞 AI 开发者大赛（challenge.xfyun.cn，经 ru41-browser 渲染抓取，赛道多、含智能体应用赛）
- 页面监控（pagewatch）：腾讯安全众测 zc.tencent.com/hackathon、腾讯广告算法大赛 algo.qq.com、百度之星 star.baidu.com
- 覆盖策略：以聚合平台为主力，企业官网只挂"一年一次"的门铃监控，不再为单个企业写解析适配器。天池不做适配器——热门赛会被 HackerTrip 二手收录（已实证：蚂蚁灵波挑战赛）
- 明确不在覆盖内：Kaggle（缺 API key）、DoraHacks（反爬拦脚本）、腾讯云黑客松（低质赛事多，用户主动放弃）、MLH（多为北美线下赛）、天池（走 HackerTrip 二手覆盖）

用户问「雷达都盯哪些平台/能不能加某某源」时按这份清单回答；加源要改 `[sources.rss]` feeds 或写新适配器，说明需要改配置，不要假装当场能加。

## 怎么回答

- 「最近有什么机会」：取最近 2-3 份日报，按主题归并（免费额度 / 竞赛 / 实习 / 行业动态），标注来自哪天的哪份报。
- 「今天有什么新的」：今天还没出报时，说明下一份报的出报时间（08:00 / 21:00），并基于最近一份日报回答；不要声称能拿到池子里未精选的实时条目。
- 已过期的机会（领取截止、报名截止已过）明确标注「已过期」，不要当成可操作项推荐。
- 日报里出现真正值得行动的机会（有奖金的比赛、限量额度、招聘窗口）时，建议用户把它落成 `05-Opportunities-机会/` 对应分类下的正式笔记（格式见 `_模板-机会.md`）——日报是流水，机会笔记才是资产。

## 纪律

- 不要手动运行 `rss poll`：它只往池子里攒条目，不会提前出报，跑了用户也看不到新东西。
- 日报内容区分事实与推断是设计口径，转述时保持这个区分，别把推断说成事实。
- 用户想调整关注方向或源列表时，说明这需要改配置（`[sources.rss]` 的 interests/feeds），不要假装能当场改。
