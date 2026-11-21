---
type: research
status: draft
language: Rust
agent: mavis
updated: 2026-11-21
tags: [Ru41, agent, adk, skills, mcp]
---

# Agent 框架调研：ADK / Skills / MCP 三层，以及 Ru41 的位置

> 起因：用户问「为什么不造 ADK 而是造 skill」，并提到「大厂安卓软件 agent 好像有 ADK」。
> 本文核实三者分别是什么、什么关系，以及 Ru41 已经有了哪些、缺哪一层。

## 0. 一句话结论

**ADK、Skills、MCP 不在同一层，不构成二选一。**

```
MCP     = agent 怎么连外部世界   （工具/数据协议）
Skills  = agent 怎么学一件事     （能力包：说明书 + 资源）
ADK     = agent 怎么被组织起来跑 （编排运行时：图、状态、人在环）
```

Ru41 **已经实现了 Skills 标准**，也**已有 ADK 的绝大多数运行时零件**，
**唯一缺的是 `Workflow` 那一层**（图式编排）。所以"造 ADK 还是造 skill"这个二选一
问错了：skill 不用造，workflow 才要造，而且不必造通用 ADK。

## 1. Google ADK 2.0

> 来源：https://github.com/google/adk-python（README，ADK 2.0）、https://github.com/google/adk-kotlin

**定位**：code-first 的 agent **编排框架**，Python / Java / Kotlin / Go / TypeScript 五语言
（Kotlin 版 `google-adk-kotlin-core` 上 Maven Central，1.6.0）。

**两个核心类**：

```python
from google.adk import Agent, Workflow

root_agent = Agent(
    name="greeting_agent",
    model="gemini-3.5-flash",
    instruction="You are a helpful assistant. Greet the user warmly.",
)

root_agent = Workflow(
    name="root_agent",
    edges=[("START", generate_fruit_agent, generate_benefit_agent)],
)
```

**Workflow Runtime**（ADK 的真正价值所在）：基于图的执行引擎，支持
- routing（路由）、fan-out / fan-in、loops
- retry、state management、dynamic nodes
- **human-in-the-loop**
- nested workflows（嵌套工作流）

**其它**：Task API（agent 间委派，支持多轮 task mode）、工具生态（内置 / 自定义函数 /
OpenAPI / **MCP tools**）、Tool Confirmation（HITL，工具执行前显式确认）、
Agent Config（YAML 建 agent，不用写代码）、Dev UI + trace、`adk eval` 评估集、
部署（Docker / Cloud Run）、OTel 遥测导出。

**发布节奏**：约双周一次。

## 2. Anthropic Agent Skills

> 来源：https://github.com/anthropics/skills（README + spec）、标准站 https://agentskills.io

**定位**：一个**目录**，含 `SKILL.md`（YAML frontmatter + 说明）+ 可选脚本/资源。
Claude **动态加载**它们，用来提升特定任务的表现。

```markdown
---
name: my-skill-name
description: A clear description of what this skill does and when to use it
---

# My Skill Name

[Add your instructions here that Claude will follow when this skill is active]
```

frontmatter 只强制两个字段：`name`（唯一标识，小写+连字符）、`description`（说清做什么 + 何时用）。
仓库里同时提供 `spec/`（规范）与 `template/`（模板），并有 skills.sh 分发。

**渐进式披露（progressive disclosure）**：agent 先只看索引（name + description），
匹配到才读完整 SKILL.md，需要时才读附属脚本/资源。

## 3. MCP 在哪一层

MCP（Model Context Protocol）是**工具与数据接入的标准协议**。
ADK 明确支持把 MCP tools 接进 agent；Skills 的 skill 也能通过 MCP 分发。

三层关系：**MCP 给 agent 装手，Skills 给 agent 装知识，ADK 给 agent 装组织方式。**

## 4. 关于「安卓 agent 的 ADK」——两个都对

1. **ADK Kotlin 确实有 Android 支持**。README 有专门章节
   *"On-device & Cloud Agents on Android"*：

   | 后端 | 模块 | 推理位置 | 工具调用 |
   |---|---|---|---|
   | LiteRT-LM | `google-adk-kotlin-litertlm` | 端侧（Gemma 等） | ✅ |
   | ML Kit | `google-adk-kotlin-mlkit-android` | 端侧（Gemini Nano） | ❌ 尚不支持（beta） |
   | Firebase AI | `google-adk-kotlin-firebase-android` | 云端 | ✅ |

   三种后端都是同一个 `Model` 接口，**换后端是一行代码**；
   还能把端侧 agent（离线/隐私敏感/低成本）与云端 agent 组合成混合多 agent 系统。
   另有 `examples/android/` 的 Compose 示例（LiteRT-LM chat / ML Kit chat / Firebase AI chat）。

2. **Android Developer Kit（SDK）** 是传统的安卓开发工具包，与 agent 无关，只是同名。

## 5. Ru41 的现状对照

| ADK / Skills 概念 | Ru41 对应物 | 状态 |
|---|---|---|
| `Agent`（instruction + tools + model） | task + prompt + backend + skill 包 | ✅ |
| **Skills 规范** | `src/skills/mod.rs`：frontmatter `name`（须等于目录名）+ `description`（必填）；`materialize()` 复制到 `run_dir/.agents/skills/<name>/`；`skills_index_prefix()` 索引注入 prompt，并要求「第一轮只读最匹配的一份 SKILL.md，不要同时执行 ls/pwd/find/--help」 | ✅ **已是标准实现** |
| Session | `conversation` kind（键 `{channel}:{target}` → `session_id`）；pi 后端按 `conversation_id` 隔离 session 目录 | ✅ |
| Memory / Artifact | `records`（kind + id + revision + body） | ✅ |
| human-in-the-loop | `interaction` kind + `open_question` / `pending_input`；`dismiss_response` 支持「提问类：用户未答复，按你的判断继续」 | ✅ |
| Tool Confirmation | `interaction` 的 options / judge | ✅ |
| MCP tools | CLI 入口（`ru41 todo list` / `ru41 jwxt classes week` 等） | ✅ 等价 |
| Task API（agent 间委派） | 无（Ru41 只有单 agent，路由靠 skill 索引 + prompt） | ⚠️ 用不上 |
| Eval | `cargo test` + `ru41 task trace` / `agent_trace` | 部分 |
| Dev UI | `ru41 task trace` | 部分 |
| **`Workflow`（图式编排）** | **缺** —— 每个插件各写一套跨轮状态机（obsidian 的 `PendingOrganize`、planner 的 `DecomposePending`） | ❌ **唯一缺口** |

### 5.1 缺口的代价（已实测）

同一模式写了两遍，且都写错过：obsidian 早就有 `PendingOrganize` + `handle_pending` 轮询，
planner 又手写一遍 `DecomposePending` + `poll_once`，后者还引入 3 个 bug
（`blocks_used` 计数、`pick_next` 判据、部分更新丢 step）。

**这就是「每插件各写编排」的真实账单。**

## 6. 所以该造什么

**不是通用 ADK**（LangGraph / Google ADK 已经在那儿，Ru41 用不上多 agent 图与动态路由，
用 Rust 重写一遍是第二个 Ru41），**而是把 ADK 的 `Workflow` 抽象搬进 Ru41**：

```yaml
# 规划账本工作流（声明式，ADK 的 edges 思想）
workflow:
  name: plan-ledger
  state_kind: plan.ledger
  steps:
    - { id: collect_evidence, skill: plan-collect-evidence }
    - { id: judge_status,    skill: plan-judge-status }
    - { id: ask_user,        hitl: true }        # 复用 interaction / open_question
    - { id: write_ledger,    skill: plan-write-ledger }
    - { id: notify }
  edges: [collect_evidence -> judge_status -> ask_user -> write_ledger -> notify]
```

宿主提供通用执行器：起任务 → 等 `interaction` 回复 → 续跑 → 落盘 → 通知。
插件不再各写状态机，只写「这一步做什么」。

**第一个用例就是这次讨论的规划闭环**：
```
采集证据 → 判定状态 → 追问（现成的 open_question）→ 写回账本 → 通知
```
每一步的能力都现成，缺的只有编排。

## 7. 来源

- Google ADK Python（2.0）：https://github.com/google/adk-python
- Google ADK Kotlin（含 Android on-device/cloud）：https://github.com/google/adk-kotlin
- ADK 文档：https://adk.dev/
- Anthropic Skills：https://github.com/anthropics/skills
- Agent Skills 标准：https://agentskills.io
- Anthropic 工程博客《Equipping agents for the real world with Agent Skills》：https://anthropic.com/engineering/equipping-agents-for-the-real-world-with-agent-skills
