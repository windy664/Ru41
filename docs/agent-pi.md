# Pi 底座

适配 Pi coding agent 1.2.0 的 JSONL RPC。参考 [官方 RPC 文档](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)，以本机 --help 和随包协议文档核对。

```toml
[agent.pi]
enabled = true
bin = "pi"
# model = "提供方/模型ID"
# provider = "提供方" # 指定 provider 时必须同时指定 model
```

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

测试覆盖正常完成、重试中间事件、失败/中止、非法结果、输出上限、取消和同一 Worker 多底座路由。
本机 Pi 1.2.0 已通过真实 get_state RPC 握手，未调用模型；模型推理和提供方认证不在离线测试验证范围内。
