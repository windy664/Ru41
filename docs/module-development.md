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

- `app/task/`：任务创建、取消、重试与查询
- `app/digest.rs`：去重、关键词过滤和 digest 入队
- `app/identity.rs`：渠道身份绑定
- `adapters/sources/feed.rs`：通用 RSS/Atom 抓取与解析
- `ingress/schedule.rs`：`run_periodic` 定时循环
- `ports/`：核心公开接口

现有范例：`modules/papers`、`modules/rss`、`modules/ecard`。

## 依赖规则

- 模块可以依赖 `domain`、`ports`、公共 `app` 用例和共享工具。
- 核心 `domain/ports/runtime/privacy` 不依赖业务模块。
- 模块之间不直接 import 对方的来源客户端或 records schema；需要共享时先提取公共能力。
- 渠道知识与默认出口选择只在 `bootstrap/assembly.rs` 转换，业务配置不点名 QQ 或飞书。
- 不擅自修改 Worker、`TaskStore`、核心 trait 或数据库迁移；确需变更时先与核心开发对齐。
- records kind 使用模块前缀（如 `arxiv_seen`），读写与 JSON schema 尽量收拢在同一个模块内。

## 新模块注册清单

### 1. 定义模块与配置

新建 `src/modules/<name>/`，并在 `src/modules/mod.rs` 添加 `pub mod <name>;`。`lib.rs` 已导出整个 `modules`，无需为每个新模块重复修改。

配置类型通常 `Deserialize + Default` 并使用 `deny_unknown_fields`。在 `bootstrap/config.rs` 的组合配置中引用该类型；保持外部 TOML 字段清晰、可选且有明确默认值。密钥优先使用环境变量，真实值只放 gitignored 的本地配置。

### 2. 在装配层构造实例

在 `bootstrap/assembly.rs` 增加 `<name>_tracker(config, store)`：

- 校验必填配置并给出带配置路径的错误信息
- 构造来源客户端与业务 Config
- 需要默认通知时复用装配层的路由助手
- 不在业务模块里硬编码 webhook、QQ target 或渠道名称

### 3. 接入 CLI 与定时执行

在 `ingress/cli.rs` 增加命令定义，在 `main.rs` 增加命令分发：

- `poll`：执行一轮并输出 JSON 报告，适合系统 cron 和调试
- `run`：调用 `ingress/schedule.rs::run_periodic`，适合常驻进程

单轮业务必须幂等；网络失败只影响本轮，不能破坏后续轮询。

### 4. 可选：注册 QQ 命令

在模块内实现 `channels/qq/commands.rs::QqCommandHandler`：

- `matches` 只判断命令词，避免误拦截自然语言
- `help` 返回一行说明，帮助文本由 QQ 动态汇总
- `handle` 返回 `Ok(Some(reply))` 表示已处理；返回 `Ok(None)` 表示不拦截，消息继续落 Agent
- 需要终态回复时，用 `CommandContext.target/msg_id` 构造 `NotifyRoute`
- 私人数据必须检查身份绑定和单聊上下文，参考 ecard 的水电命令

然后在 `bootstrap/assembly.rs::qq_command_handlers` 注册 handler。QQ 渠道只遍历 handler，不应 import 新模块。

### 5. 可选：提供通知渠道

新渠道实现 `ports::channel::NotifyChannel`，代码放 `src/channels/<name>/`，由装配层注册给 `NotifyDispatcher`。业务模块只拿到 `NotifyRoute`，不自行决定默认出口。

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
