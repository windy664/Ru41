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
- [x] 装配层收口：附属装配集中在 `bootstrap/assembly.rs`（渠道知识唯一驻地），时间源 `now()` 上移 `util`（worker 保留 re-export）
- [x] 业务附属垂直模块化：`modules/papers`、`modules/rss`、`modules/ecard` 各自持有配置、来源与业务，论文/水电同时持有 QQ 命令；QQ 通过 `QqCommandHandler` 注册表扩展，不直接依赖业务模块
- [x] 消息渠道垂直收拢：`channels/qq` 集中机器人、REST/WebSocket 协议与命令接口，`channels/feishu.rs` 持有 webhook 通知；原 messaging 适配目录已移除
- [x] Docker 部署：多阶段单镜像（内置 Codex）、Compose 分进程/profile、宿主机持久化与 SQLite 在线备份；Pi/浏览器等重依赖后续按独立镜像扩展
- [x] CLI 运维命令：幂等 `init`、离线/深度 `doctor`、脱敏 `config show` 与 `dump`、`config check`、`qq doctor`、数据库聚合 `status`

## 核心后续

- [ ] 任务级暂停、结构化错误分类、预算与日志保留策略
- [ ] 工具执行契约与版本绑定的业务审核
- [ ] 持久化投递队列和事件入口
- [ ] 会话恢复；第二种真实 Agent 底座按实际需求接入

## P1 Obsidian 闭环

- [ ] Git 增量扫描、输入版本与处理游标
- [ ] Agent 生成归档 ChangeSet
- [ ] 单一 Vault 写入服务、内容指纹复核、移动与链接修复
- [ ] Git 提交、推送失败恢复与自身提交去重
- [ ] 临时 Vault 验证并发编辑、重复事件和崩溃恢复

## P1 信息与消息

- [x] 论文来源采集：arXiv 定时轮询、按 ID 去重、关键词粗筛、Codex 每日精选 digest 与推送（来源笔记落 Vault 待 Obsidian 闭环）
- [x] RSS 通用来源采集：RSS 2.0 与 Atom 订阅源轮询、按源+guid/id 去重、四段式精选 digest（多源事件聚合后续）
- [x] 水电费监控：ecard 平台接口逆向（docs/ecard-waterfee.md）、每日余额采集落库、低余额阈值提醒（充值识别与消耗趋势后续）
- [x] 一个消息渠道（QQ 官方机器人）：记录想法入队任务、终态结果回复、固定命令（帮助/任务查询/取消）与口令身份绑定

## 后续

水电、知到、X 情报、周报、机会匹配、财务和媒体。先跑通两个真实业务，再决定 crate 拆分、模块接口和插件安装机制。
