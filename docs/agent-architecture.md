# Agent 接入架构

Ru41 托管外部 Agent 应用。任务执行依赖统一端口，具体通信协议属于适配器；启动配置的组合属于 bootstrap。

```text
src/
├── ports/agent.rs             # AgentBackend、请求/结果、能力描述、注册表
├── bootstrap/
│   ├── agents.rs              # 配置与 CLI 覆盖 → 注册表
│   └── config.rs              # 配置文件加载与配置入口
├── adapters/agent/
│   ├── mod.rs                # 导出各底座模块
│   ├── process.rs            # 内部进程清理工具
│   ├── codex/
│   │   ├── mod.rs            # Codex 对外类型
│   │   ├── exec.rs           # codex exec 协议
│   │   ├── session.rs        # app-server 协议及审核转发
│   │   └── provider.rs       # Codex 模型提供方参数及校验
│   └── pi/
│       ├── mod.rs            # Pi 对外类型
│       └── rpc.rs            # Pi RPC 协议
└── runtime/worker.rs         # 通用任务执行
```

`codex` 与 `codex-session` 是同一应用的两种接入模式，保留已有路由 ID。Pi 的路由 ID 是 `pi`。模块的对外入口是 `agent::codex::{CodexAgent, CodexSession}` 和 `agent::pi::PiAgent`；底层协议模块不公开。

`main.rs` 把启动配置和 CLI 覆盖交给 `bootstrap::agents::registry`，不再构造各底座实例。现有 `--model`、`--codex-bin` 仅覆盖 Codex，不能影响 Pi。Codex 提供方参数类型归 Codex 模块所有，配置入口再导出该类型，适配器不反向依赖 bootstrap。

新增底座：在 adapters/agent 下添加独立模块，实现 AgentBackend，再增加配置和 bootstrap 注册。Worker、业务模块和数据库无需了解新协议。目前为编译时模块化，不是动态插件加载。

各底座只声明实际适配的能力。进程清理可以共用，但完成事件、审核消息和会话语义由各协议自己解释。Pi 首版仍仅支持 analysis_v1；本次目录整理不增加能力。
