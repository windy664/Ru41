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
- [x] 自动行为测试及一次真实 Codex 分析任务验证

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

- [ ] 论文/RSS 来源记录、筛选和来源笔记
- [x] 一个消息渠道（QQ 官方机器人）：记录想法入队任务、终态结果回复；身份绑定与任务查询指令后续补充

## 后续

水电、知到、X 情报、周报、机会匹配、财务和媒体。先跑通两个真实业务，再决定 crate 拆分、模块接口和插件安装机制。
