# Ru41 文档索引

项目入口与快速开始见根目录 [README](../README.md)。实现状态以代码和[核心宿主状态](core-status.md)为准，规划文档不代表能力已经实现。

- [使用与配置手册](usage.md)：完整命令、配置、部署与 HTTP API 示例

## 产品、状态与规划

- [产品设计](product-design.md)：产品目标、业务场景与总体设计
- [开发路线](roadmap.md)：已完成能力与后续阶段
- [核心宿主状态](core-status.md)：核心边界、并行开发约定和未完成项
- [附属模块开发指南](module-development.md)：垂直模块目录、静态装配、QQ 命令与测试清单
- [Docker 部署](../deploy/README.md)：单镜像多进程、持久化数据、备份恢复与重依赖扩展

## Agent 与核心运行时

- [Agent 架构](agent-architecture.md)：底座注册、运行与宿主职责
- [Agent 底座接入](agent-backends.md)：新增底座的契约与步骤
- [Pi 底座](agent-pi.md)：Pi JSONL RPC 的配置与首版边界
- [持久化事件与 SSE](events.md)：事件类型、游标续传和持久化边界
- [可靠事件消费者](event-consumers.md)：确认、租约、重试、恢复与运维 API

## 消息渠道

- [QQ 渠道](channels-qq.md)：WebSocket/REST、消息幂等、内置与模块命令、身份绑定
- [飞书渠道](channels-feishu.md)：自定义群机器人 webhook 与默认通知出口

## 来源与业务模块

- [arXiv 论文追踪](sources-arxiv.md)：论文 digest、按 ID 精读与配置
- [RSS 信息流](sources-rss.md)：RSS/Atom 采集、去重和精选输出
- [校园卡水电监控](ecard-waterfee.md)：协议调查、读数采集、提醒与平台限制
- [智慧树作业提醒](sources-zhihuishu.md)：sidecar 报告契约、deadline 分类与考试式红线
- [学习通签到监控](sources-chaoxing.md)：协议端直连、待签提醒与手动签到红线
- [教务课表监控](sources-jwxt.md)：CAS 单点登录拉课表、变更检测与明日课表提醒

## 推荐阅读顺序

1. 根目录 README：快速开始、配置与整体架构
2. 核心宿主状态：确认哪些能力尚未完成
3. 对应渠道或来源文档：搭建并运行一个真实闭环
4. 附属模块开发指南：在不修改核心的前提下扩展新业务
