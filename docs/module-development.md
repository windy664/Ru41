# 附属模块开发指南

Ru41 的附属模块采用**垂直业务目录 + 编译期静态装配**。模块自己持有配置、外部来源、业务逻辑和可选渠道命令；核心不反向 import 模块。当前不是动态插件系统，新增模块仍需在组合根显式注册。

## 目录约定

以 papers 为参考：

```text
src/modules/<name>/
├── mod.rs          # 模块导出
├── config.rs       # TOML Section；字段 Option，默认值在装配时补齐
├── source.rs       # 可选：外部 API / 协议客户端，不写任务流程
├── service.rs      # 业务编排、幂等键、prompt、报告
└── qq_command.rs   # 可选：QQ 命令，放在模块而不是 QQ 渠道里
```

可复用的公共能力放在原位置：

- `app/task/`：Agent 执行任务的创建、取消、重试与查询
- `app/todo.rs`：私人常驻待办与今日/近期/长期/逾期视图
- `app/schedule.rs`：行程（时间轴占用）与待办互补：一次性/每周重复事件、读取时展开、冲突检测、到点提醒。附属直接 `ScheduleService::new(store, options)` 构造使用（zhihuishu 写作业截止、chaoxing 灌课表都是这个路子）；跨进程访问方（agent、sidecar）走 `ru41 event ...` CLI
- `app/obsidian.rs`：Obsidian Vault 核心服务（轮询游标、整理任务编排、提交推送）
- `app/digest.rs`：去重、关键词过滤和 digest 入队
- `app/identity.rs`：渠道身份绑定
- `ports/vault.rs` + `adapters/vault.rs`：Vault 读写端口与 git 实现，附属经 `Arc<dyn Vault>` 读写 Vault
- `adapters/sources/feed.rs`：通用 RSS/Atom 抓取与解析
- `ingress/schedule.rs`：`run_periodic` 定时循环与 `PollJob` trait
- `ports/`：核心公开接口

现有范例：自包含附属 `modules/papers`、`modules/rss`、`modules/ecard`、`modules/projects`（GitHub 项目库同步）；薄门面附属 `modules/planner`（QQ 待办/日程入口，持久化由核心 `app/todo.rs` 与 `app/schedule.rs` 提供）、`modules/obsidian`（QQ 记一下 + 归档技能，Vault 读写由核心 `app/obsidian.rs` 提供）。

## 两种附属形态与升级路径

**自包含附属**（papers/rss/ecard）：模块持有配置、来源客户端、业务逻辑，其他模块不依赖它。

**共享能力附属**（todo/obsidian）：能力本身是多个附属的公共依赖（digest 落 Vault、周报都要写 Vault；待办提醒、知到作业都要写 Todo）。这时按「核心服务 + 薄附属门面」组织：

```
核心：ports/<cap>.rs（端口）+ adapters/<cap>.rs（实现）+ app/<cap>.rs（服务）+ bootstrap/config.rs（顶层配置段）
附属：modules/<cap>/ 只剩 qq_command.rs 与 skills/，业务逻辑不进模块
```

**升级判据**（满足其一就该升）：第二个附属需要调用它；它涉及确定性管道 + 私人数据（不能让每个附属各写一遍）；它需要跨附属的统一幂等/串行（如 Vault 的 flock）。升级清单：定义端口 → 实现移到 adapters → 服务移到 app（构造函数注入 `Arc<dyn Port>`）→ 配置升为顶层段（与 `[todo]` 同级）→ `modules/` 只留渠道入口与技能 → 装配层改为构造核心服务、门面复用同一实例配置。`[sources.*]` 只留给真正业务来源（arxiv/rss/ecard/zhihuishu）。

## 依赖规则

- 模块可以依赖 `domain`、`ports`、公共 `app` 用例和共享工具。
- 核心 `domain/ports/runtime/privacy` 不依赖业务模块。
- 模块之间不直接 import 对方的来源客户端或 records schema；需要共享时先提取公共能力（按上面的升级路径进核心端口，如 `ports/vault`）。
- 渠道知识与默认出口选择只在 `bootstrap/assembly.rs` 转换，业务配置不点名 QQ 或飞书。
- 不擅自修改 Worker、`TaskStore`、核心 trait 或数据库迁移；确需变更时先与核心开发对齐。
- records kind 使用模块前缀（如 `arxiv_seen`），读写与 JSON schema 尽量收拢在同一个模块内；跨模块共享的 records kind（如 `todo_item`、`obsidian_state`）归核心服务所有。

## 新模块注册清单

### 1. 定义模块与配置

新建 `src/modules/<name>/`，并在 `src/modules/mod.rs` 添加 `pub mod <name>;`。`lib.rs` 已导出整个 `modules`，无需为每个新模块重复修改。

配置类型通常 `Deserialize + Default` 并使用 `deny_unknown_fields`。在 `bootstrap/config.rs` 的组合配置中引用该类型；保持外部 TOML 字段清晰、可选且有明确默认值。密钥优先使用环境变量，真实值只放 gitignored 的本地配置。自包含附属放 `[sources.<name>]`；共享能力按「升级路径」放顶层段（如 `[todo]`、`[obsidian]`）。

### 2. 在装配层构造实例

在 `bootstrap/assembly.rs` 增加 `<name>_tracker(config, store)`：

- 校验必填配置并给出带配置路径的错误信息
- 构造来源客户端与业务 Config
- 需要默认通知时复用装配层的路由助手：`notify_pair(config, section.notify)` 一次拿到 `(notifier, target)`；需要 Obsidian 同步时用 `vault_if_ready(config, section.vault_sync)` 接线 Vault
- 不在业务模块里硬编码 webhook、QQ target 或渠道名称

### 3. 接入 CLI 与定时执行

在 `ingress/cli.rs` 增加命令定义，在 `main.rs` 增加命令分发：

- main.rs 只做参数路由；模块专属的处理逻辑放 `modules/<name>/cli.rs`（参考 jwxt 的 `classes_json`/`login_json`），核心入口不得认识附属内部
- `poll`：执行一轮并输出 JSON 报告，适合系统 cron 和调试
- `run`：调用 `ingress/schedule.rs::run_periodic`，适合常驻进程

同时在 `bootstrap/assembly.rs::scheduled_jobs` 用 `job()` 构造器登记一个条目（模块、任务名、间隔、启用条件）；poll/run 命令按 `{name} poll|run` 约定自动生成，不合约定的用结构体更新语法覆盖。登记后任务自动出现在 `ru41 schedule list`，并可被 `ru41 schedule run <name>` 统一启动；这是注册表唯一需要改动的位置。

如果模块提供轮询 tracker，还需两步：

- 在 `XxxSection` 上实现 `resolved()` 把默认值集中在一处（间隔、阈值等常量定义为 section 关联常量），装配层与 schedule list 共用，避免两处默认值漂移；
- 为核心 trait `ingress/schedule.rs::PollJob` 实现 `job_interval()` 与 `poll_json()`，模块 `run` 命令与 `schedule run` 即自动共用调度循环，入口侧不再重复构造。

单轮业务必须幂等；网络失败只影响本轮，不能破坏后续轮询。

### 4. 可选：注册 QQ 命令

在模块内实现核心扩展点 `ports/commands.rs::CommandHandler`（渠道无关，未来接入其他渠道命令体系时复用同一 trait）：

- `matches` 只判断命令词，避免误拦截自然语言
- `help` 返回一行说明，帮助文本由 QQ 动态汇总
- `handle` 返回 `Ok(Some(reply))` 表示已处理；返回 `Ok(None)` 表示不拦截，消息继续落 Agent
- 可选实现 `handle_message` 处理自然语言；默认不拦截。只有模块已完成身份、单聊和业务授权校验后，才应向 Agent 任务注入私人数据
- 需要终态回复时，用 `CommandContext::reply_route()` 构造 `NotifyRoute`（渠道字面量由渠道适配器注入 `reply_channel`，模块内不得手写渠道名）
- 私人数据用 `CommandContext::require_private(action, privacy_note)` 统一双闸（身份绑定 + 单聊），参考 ecard 的水电命令

然后在 `bootstrap/assembly.rs::qq_command_handlers` 注册 handler。QQ 渠道只遍历 handler，不应 import 新模块。

### 5. 可选：提供通知渠道

新渠道实现 `ports::channel::NotifyChannel`，代码放 `src/channels/<name>/`，由装配层注册给 `NotifyDispatcher`。业务模块只拿到 `NotifyRoute`，不自行决定默认出口。

## 可选：提供 Agent Skills

技能型附属只需添加技能包，不修改 QQ、Worker 或核心 trait：

```text
src/modules/<name>/
└── skills/
    └── <skill-name>/
        ├── SKILL.md       # 必需：name、description 和具体工作流
        ├── references/    # 可选：补充文档
        └── assets/        # 可选：模板或资源
```

- `name` 必须与目录名一致且使用小写字母、数字和连字符；不同模块不能使用同名技能。
- 装配时按 `root/<module>/skills/<skill>/SKILL.md` 自动发现。默认 roots 是 `src/modules` 与 `/etc/ru41/modules`，可用 `[agent.skills] roots` 覆盖。
- Codex 在每次任务目录生成 `.agents/skills/<skill>/`，使用原生按需加载；不支持原生技能的底座（当前 Pi）改为完整提示词注入。
- 技能型附属不需要修改 `src/modules/mod.rs`。若同时提供 CLI、定时任务或 QQ 命令，仍须按前面的代码型注册清单在组合根注册。
- 禁止符号链接、路径穿越和重复技能名；限制单文件、文件数和技能包总大小。不要把密钥、个人配置或真实隐私数据放入技能包。
- 更新技能即更新对应文件并重新运行任务。Skill-RSI champion 也只有经过人工审查和显式复制后才能成为附属，不能自动进入生产。

## 测试要求

新增或更新与模块同名的 integration test，至少覆盖：

- 协议解析：正常、缺字段、错误响应
- 单轮业务：成功、无候选、幂等重复、来源失败
- 通知路由：配置出口正确传递，业务不点名渠道
- QQ 命令：别名、非法参数不拦截、权限与隐私边界
- 配置：TOML 字段能解析，未知字段仍被拒绝

外部 HTTP 服务使用本地 mock，不在自动测试中消耗真实模型额度或访问私人服务。提交前必须通过：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

同时检查 `git diff --check`、敏感信息和核心目录 diff。模块文档至少说明：配置、运行方式、幂等键、通知路径、隐私边界和尚未实现的限制。
