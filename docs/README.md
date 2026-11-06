# Ru41 文档索引

项目入口与快速开始见根目录 [README](../README.md)。实现状态以代码和[核心宿主状态](core-status.md)为准，规划文档不代表能力已经实现。

- [使用与配置手册](usage.md)：完整命令、配置、部署与 HTTP API 示例
- [凭证与密钥总览](credentials.md)：cookie/token/key 的注入路径、环境变量清单与更换方法
- **[开发者上手（接手指南）](onboarding.md)：从 clone 到第一次插件上线的一半天路径、生产拓扑速查与排障表**

## 产品、状态与规划

- [产品设计](product-design.md)：产品目标、业务场景与总体设计
- [开发路线](roadmap.md)：已完成能力与后续阶段
- [核心宿主状态](core-status.md)：核心边界、并行开发约定和未完成项
- [动态插件开发指南](plugin-development.md)：新附属的默认路线——cdylib 热插拔、ru41-plugin-api 边界与握手纪律
- [内建模块开发指南](module-development.md)：**历史参考**——静态装配路线已随 2026-11-03 全量插件化弃用，仅当将来恢复内建形态时再看
- [Docker 部署](../deploy/README.md)：单镜像多进程、持久化数据、备份恢复与重依赖扩展
- [工程约定](conventions.md)：目录边界、构建踩坑（共享 target、bridge、plugin-entry feature）、部署分层与红线——**上手开发前必读**

## 设计研究（待实施）

- [敏感信息与 LLM 边界](privacy-llm-boundary.md)：脱敏现状缺口（agent 自主读盘不脱敏）、三色分级判据与 P0-P3 路线
- [容器故障自愈](self-healing.md)：进程/看门狗/agent 诊断三层设计，agent 永不持有 docker.sock

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
- [恒源云 GPU 库存监控](sources-compshare.md)：light-gpu 容量接口、P40 有货提醒与会话过期告警
- [微助教签到与作业](sources-teachermate.md)：openid 直连、作业临期/过期提醒（每日一次）
- [课堂录音转写](sources-lecture.md)：Paraformer 自托管 ASR、笔记落课程目录
- [图书馆检索逆向](library-recon.md)：libsp 检索协议、馆藏定位与追书

## 插件清单（23 个，均在 `plugins/`）

新开发者从这里对照找代码；标 📄 的有独立文档。

| 插件 | 职责 | 备注 |
| :--- | :--- | :--- |
| 📄 papers | arXiv 论文 digest 与精读 | sources-arxiv.md |
| 📄 rss | RSS/Atom 信息流精选 | sources-rss.md |
| 📄 ecard | 校园卡余额与水电读数、低额提醒 | ecard-waterfee.md |
| 📄 zhihuishu | 智慧树任务/作业监控 | sources-zhihuishu.md |
| 📄 chaoxing | 学习通签到 + 作业监控 | sources-chaoxing.md |
| 📄 teachermate | 微助教签到 + 作业监控 | sources-teachermate.md |
| 📄 jwxt | 教务课表拉取、调停课、每日课表推送 | sources-jwxt.md |
| 📄 compshare | 恒源云 GPU 有货监控 | sources-compshare.md |
| 📄 lecture | 课堂录音转写整理为课程笔记 | sources-lecture.md |
| 📄 library | 图书馆藏检索、找书、追书 | library-recon.md |
| homework | 三平台作业聚合 → 学期「作业总表.md」 | 与 zhihuishu/chaoxing/teachermate 快照字段对齐 |
| events | 活动/讲座/论坛日程抓取 → Obsidian | 偏科技/AI/安全，大湾区优先 |
| notices | 通知规则抽取（标题+时间+地点锚点，不走 LLM） | 课群通知 → 行程 |
| blog | Obsidian 博客发布（草稿→已发布→外部 Pages） | vault git 闭环 |
| obsidian | Vault 同步与知识库索引容器 | 索引走 cron 03:23 宿主 CLI |
| planner | 日程/行程与待办 QQ 命令 | 加行程带可取消按钮 |
| youtube | YouTube 新视频字幕预习单 | 触网 handler 已过桥 |
| jobs | 实习/校招技术岗（大湾区优先）筛选 | |
| programs | OSPP 等年度窗口项目提醒 | 无公开 API，静态节奏 |
| research | 科研项目（中科院科创等）窗口提醒 | |
| competitions | 竞赛信息多源抓取 → Obsidian | |
| projects | GitHub 个人项目动态归档 | |
| pagewatch | 页面变化监控 | |

## 推荐阅读顺序

**赶时间直接看 [onboarding.md](onboarding.md)**——半天接手路径 + 排障速查表。完整顺序：

1. 根目录 README：快速开始、配置与整体架构
2. [工程约定](conventions.md)：构建踩坑与协作契约（插件不在 workspace、共享 target、bridge 纪律、部署分层）
3. 核心宿主状态：确认哪些能力尚未完成
4. 上表找到对应插件 → 渠道或来源文档：搭建并运行一个真实闭环
5. 动态插件开发指南：在不修改核心的前提下扩展新业务（独立 crate 热插拔）
