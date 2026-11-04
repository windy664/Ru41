# Pi 底座

适配 Pi coding agent 1.2.0 的 JSONL RPC。参考 [官方 RPC 文档](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)，以本机 --help 和随包协议文档核对。

```toml
[agent.pi]
enabled = true
bin = "pi"
# model = "提供方/模型ID"
# provider = "提供方" # 指定 provider 时必须同时指定 model
# base_url = "https://example.com/anthropic" # 自定义兼容端点
# api = "anthropic-messages" # anthropic-messages / openai-completions / openai-responses
# api_key_env = "MINIMAX_API_KEY" # 密钥来源环境变量；值只进子进程环境
# session = true # 会话持久化，可按 session_id 恢复
# session_dir = "/data/pi-sessions"
# tools = "read,write,edit,bash" # 工具白名单；留空则禁用全部工具
```

切底座不需要改核心：QQ 入口的底座由 `[channel.qq] backend` 决定，
填 `pi` 即走 Pi；worker 侧只需在 `[agent.pi]` 里配好模型与密钥变量。

```sh
ru41 --config ru41.toml backends
ru41 --config ru41.toml task add --backend pi --key pi:example:1 --prompt "总结下面提供的材料……"
ru41 --config ru41.toml worker --once
```

认证、模型提供方和自定义 API 配置由 Pi 原生环境变量与配置管理，Ru41 不复制密钥。启用后同一 Worker 注册表包含 Codex 与 Pi；默认不启用 Pi。

## 首版范围

- 只实现 analysis_v1：输入脱敏后的文本，输出经校验的 JSON summary。
- 不把 prompt 接受回执或 agent_end 当作完成；等待 agent_settled，再验证最终 assistant message 的 stopReason 和内容。
- 无会话持久化、无工具、无扩展/MCP/技能/上下文文件发现。不声明审批或恢复能力；遇到 extension_ui_request 明确失败，不自动授权。
- 超时、取消、租约与回填复用 Worker；取消时终止进程组并回收直接子进程。
- stdout 按有界 JSONL 读取，stderr 持续排空并限制总量。
- 关闭工具属于 Pi 启动配置，不是独立的操作系统沙箱；不可信可执行程序仍可能读取宿主环境。

## 与 Codex 底座的对等项

两个底座共用 `AgentBackend` 抽象（契约、描述符、结果与用量口径一致），差异只在协议：

- **token 用量**：Pi 在 `message_update` 事件里上报累计用量（input/output/cacheRead/totalTokens），
  适配器折算成统一的 `TokenUsage`；Codex 走 `token_count` 事件，字段名不同但口径一致。
- **自定义端点**：`base_url` + `api` 写进运行目录下的临时 agent 目录（`models.json`），
  Pi 通过 `PI_CODING_AGENT_DIR` 读取；密钥只以 `$ENV` 引用出现在文件里，真实值只留在子进程环境。
- **密钥**：`api_key_env` 指定变量名，宿主从自身环境取值注入子进程；配置、日志、错误信息里只有变量名。
  Pi 原生 provider（如 MiniMax 用 `MINIMAX_API_KEY`）也直接认同名环境变量。
- **会话与恢复**：`session = true` 时不再传 `--no-session`，运行结束通过 `get_state` 取会话 ID，
  后续按 `resume_session_id` 以 `--session <id>` 续跑；会话底座单独注册为 `pi-session`。
- **网络**：不再强制 `--offline`——那会挡住模型调用；离网需求由部署环境的出口策略保证。
- **用量分摊**：底座上报「实际使用的模型」，用量按模型聚合（`/用量` 的「7 天分模型」、
  `ru41 task usage` 的 `by_model`）。换供应方或走 fallback 后，账目仍然对得上。
- **fallback 链**：`fallback_models = ["供应方/模型", ...]` 列出备用模型；主模型因限流、
  服务端错误或协议中断失败时按顺序切换，配置类/契约类错误不切换（换模型也救不了）。
  pi 自身没有跨供应商 fallback（只有同模型的自动重试），链路由 Ru41 侧实现；
  实际生效的模型会回写 attempts.model，用量据此分摊。

## 测试覆盖

测试覆盖正常完成、重试中间事件、失败/中止、非法结果、输出上限、取消和同一 Worker 多底座路由。
本机 Pi 1.2.0 已通过真实 get_state RPC 握手，未调用模型；模型推理和提供方认证不在离线测试验证范围内。
