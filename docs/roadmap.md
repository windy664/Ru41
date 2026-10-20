# 开发路线

## P0 已完成

- [x] Rust CLI、TaskService、SQLite 持久化任务与重复请求检查
- [x] Worker 原子领取、租约续期、取消、退避重试与过期恢复
- [x] TaskAttempt 历史、Agent 输出和本地运行日志
- [x] Codex CLI 运行接入、JSONL 与结构化结果校验、超时清理
- [x] 通用 AgentBackend、启动注册、任务按底座路由
- [x] 数据库 v1/v2 到 v3 的迁移
- [x] TaskStore 存储抽象，SQLite 为默认实现，PostgreSQL 适配器（`--store postgres`，同步 `postgres` crate 驱动）
- [x] 任务终态通知路由（notify_channel/target/msg_id/notified_at）与 QQ 官方机器人消息网关（收消息幂等入队、终态被动回复）
- [x] Agent 调用前信息脱敏管线（随机同类假数据替换 + 结果回填，映射表仅存本地尝试目录）
- [x] Worker 按任务 backend_id 路由底座（持有 BackendRegistry，移除 `worker --backend`）
- [x] TOML 配置体系（`--config`/./ru41.toml/ ~/.config，CLI > 环境变量 > 配置文件 > 默认值）
- [x] 终态通知 dispatcher 上移至应用层（NotifyChannel trait + NotifyDispatcher，QQ 为首个渠道实现）
- [x] 飞书自定义群机器人 webhook 通知渠道（`[notify]` 默认出口统一路由，`ru41 notify` 独立派发进程）
- [x] 自动行为测试及一次真实 Codex 分析任务验证
- [x] 定时器入口统一机制：`ingress/schedule.rs` 的 `run_periodic`（名称 + 间隔 + 轮询闭包），papers/rss/ecard 的 run 命令共用（设计文档 8.2 定时器入口；核心组如需收进 runtime 可平移）
- [x] 定时任务注册表：`ScheduledJob` 描述 + 装配层 `scheduled_jobs()` 汇总，`ru41 schedule list/run` 统一查看和启动各附属轮询任务（`tests/schedule_jobs.rs`）
- [x] 架构收口重构：附属模块不再手写 QQ 渠道字面量（`CommandContext::reply_route`）；`PollJob` trait 统一四个 tracker 的调度入口；`XxxSection::resolved()` 集中默认值；todo 提醒格式化与发送下沉 `domain`/`app`；出站 HTTP 统一 `util::http_client()`（30s 超时）；QQ 通路同步存储 I/O 经 `block_in_place` 让出 executor 线程
- [x] 身份绑定限流：连续 5 次口令错误锁定 10 分钟（锁定期间不校验口令，防枚举），窗口过期自动重置，成功清零计数（`app/identity.rs`，`tests/identity.rs`）
- [x] QQ 网关健壮性：连接与所有写操作带超时，心跳 ACK 活性检测（2 个周期无 ACK 主动重连，半开连接不再拖到 TCP 超时）
- [x] 解析层抽象：RSS/Atom 与 arXiv 共用 `adapters::sources::feed::parse_items` 事件骨架，新来源只需实现 `XmlEntry` 标签映射
- [x] 装配层收口：附属装配集中在 `bootstrap/assembly.rs`（渠道知识唯一驻地），时间源 `now()` 上移 `util`（worker 保留 re-export）
- [x] 业务附属垂直模块化：`modules/papers`、`modules/rss`、`modules/ecard` 各自持有配置、来源与业务，论文/水电同时持有 QQ 命令；QQ 通过 `ports::commands::CommandHandler` 注册表扩展，不直接依赖业务模块
- [x] 消息渠道垂直收拢：`channels/qq` 集中机器人、REST/WebSocket 协议与命令接口，`channels/feishu.rs` 持有 webhook 通知；原 messaging 适配目录已移除
- [x] Docker 部署：多阶段单镜像（内置 Codex）、Compose 分进程/profile、宿主机持久化与 SQLite 在线备份；Pi/浏览器等重依赖后续按独立镜像扩展
- [x] CLI 运维命令：幂等 `init`、离线/深度 `doctor`、脱敏 `config show` 与 `dump`、`config check`、官方扫码 `qq onboard`、`qq doctor`、数据库聚合 `status`
- [x] 私人常驻待办：核心 `TodoService`、通用 records 持久化、今日/近期/长期/逾期/收件箱视图、CLI 与 QQ `/待办` 命令、到点主动提醒与成功送达去重（循环待办和跨模块写入后续）

## 核心后续

- [ ] 任务级暂停、结构化错误分类、预算与日志保留策略
- [ ] 工具执行契约与版本绑定的业务审核
- [ ] 持久化投递队列和事件入口
- [ ] 会话恢复；第二种真实 Agent 底座按实际需求接入

## P1 Obsidian 闭环

- [x] Git 增量扫描、输入版本与处理游标（`modules/obsidian`：fetch + processed_sha 游标 + pending 任务态，远端前进时放弃重做）
- [x] Agent 归档任务：`obsidian-organize` skill + 幂等键 `obsidian:organize:<sha>`，Inbox 变更入队 Worker 整理
- [x] 提交推送与自身提交去重（`[ru41]` 消息前缀标记，不依赖 git 身份）；QQ `/记一下` 直接追加 Inbox 并推送
- [ ] 内容指纹复核（整理前记录文件 hash，提交前校验人工未改动；当前以「远端 HEAD 不变」代替）
- [ ] 临时 Vault 验证并发编辑、重复事件和崩溃恢复（已有本地 git 仓库测试 `tests/obsidian.rs`，端到端联调待做）

## P1 信息与消息

- [x] 论文来源采集：arXiv 定时轮询、按 ID 去重、关键词粗筛、Codex 每日精选 digest 与推送（来源笔记落 Vault 待 Obsidian 闭环）
- [x] RSS 通用来源采集：RSS 2.0 与 Atom 订阅源轮询、按源+guid/id 去重、四段式精选 digest（多源事件聚合后续）
- [x] 水电费监控：ecard 平台接口逆向（docs/ecard-waterfee.md）、每日余额采集落库、低余额阈值提醒（充值识别与消耗趋势后续）
- [x] 一个消息渠道（QQ 官方机器人）：记录想法入队任务、终态结果回复、固定命令（帮助/任务查询/取消）与口令身份绑定

## 后续

水电、知到、X 情报、周报、机会匹配、财务和媒体。先跑通两个真实业务，再决定 crate 拆分、模块接口和插件安装机制。

## P2 附属 backlog（自 Vault 06-Automation 迁入）

原 Vault `06-Automation-自动化/99-automation-backlog.md` 的待做清单，Vault 设计文档已随实现删除，想法保留在此：

- [x] 自动论文追踪（papers 模块）
- [ ] GitHub Star 变化监控（对应 07-github-project-sync 设计）
- [ ] AI 安全新闻监控（rss 模块加源即可）
- [ ] 实习机会匹配（对应 03-opportunity-crawler 设计）
- [ ] 比赛报名提醒（对应 04-competition-crawler 设计）
- [ ] 课程复习系统（考试前自动生成 Quiz，新设计）
- [ ] Blog 自动部署（GitHub Pages，新设计）
- [ ] 项目周报生成（对应 08-weekly-summary 设计，可结合 Obsidian 闭环）
- [ ] GitHub 贡献统计（新设计）
- [ ] 安全情报管线（CVE / Advisory / PoC，新设计）
- [ ] 月度复盘自动生成（新设计，可结合 Daily 日志自动化）

## P2 博客管线

- [x] 发布管线：`06-Blog-博客/Pending-待发布/` 即发布入口（Vault → 博客仓库单向），frontmatter 校验对齐 Astro content 集合（categories 枚举/tags≤8），规范命名 `YYYY-MM-DD-slug.md`，推送后移入 `Published-已发布/` 并 `[ru41]` 回写 Vault（`tests/blog.rs`）
- [ ] GitHub 推送凭据（PAT 经 git credential helper 注入服务器）
- [ ] 已发布目录与博客仓库 _posts 的自动对齐（当前为一次性拷入）
