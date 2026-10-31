# Agent 记忆系统调研（2026-11）

调研主流 agent 底座的记忆做法，结论映射到 ru41 的落地路径。

## 业界四种流派

### 1. AGENTS.md 约定（静态指令层）

[agents.md](https://agents.md)：60k+ 开源项目采用，Linux 基金会托管。
本质是「给 agent 的 README」：构建命令、约定、红线，纯 Markdown 无 schema。
关键设计：**分层就近覆盖**——monorepo 每个子目录可放自己的 AGENTS.md，
agent 读离被改文件最近的那个；用户对话指令永远优先于文件。
定位：**不是记忆，是入职手册**——由人（或 agent 在人的授意下）维护，
变化慢、必须精炼。Codex / Kimi Code / Claude Code / Cursor 全部兼容。

### 2. Claude Code auto-memory（自我回写）

Claude Code 的 [auto-memory](https://www.mindstudio.ai/blog/what-is-claude-code-auto-memory)：
会话中遇到「文档没写、查了才知道」的东西，**agent 自己写回 CLAUDE.md**，
下个会话开局即知。判据四条：

- 复现概率高（这次查了，下次大概率还要查）
- 不可从代码/环境推断（只能撞上才知道）
- 稳定（活跃变动中的代码不值得记）
- 项目特异（通用知识不进记忆）

要点：**记忆必须克制**。目标是「越来越准」而不是「越来越多」；
错条目会在之后每个会话复利放大，需要定期人工审计。

### 3. Anthropic memory tool（文件式记忆目录）

[memory tool](https://docs.claude.com/en/docs/agents-and-tools/tool-use/memory-tool)：
给 agent 一个 `/memories` 目录和 view/create/str_replace/delete/rename 工具，
**just-in-time 读取**——不全部塞进 prompt，按需翻文件。
官方安全要求：路径穿越校验、文件大小上限、过期清理、写入前脱敏。
配套模式（多会话工程）：开局读进度文件收上下文，收尾前更新进度日志——
记忆目录成为「下次会话从上次结束处继续」的恢复机制。

### 4. Letta/MemGPT 与 mem0（数据库记忆层）

- [Letta](https://docs.letta.com/guides/agents/memory)（MemGPT 论文实现）：
  两层——**core memory block 钉在 system prompt 里**（agent 用工具自编辑，
  永远在上下文中）+ 一切消息/推理持久化在 DB，出上下文的部分用
  archival（向量检索）/recall（对话历史）工具取回。
- [mem0](https://mem0.ai/library/agent-memory/open-source-ai-agents-with-built-in-memory)：
  独立记忆层服务——LLM 从对话抽取显著事实，与已有记忆合并决策
  （ADD/UPDATE/DELETE），存向量库，暴露 add/search API。

## 共同结论：两层记忆是共识

所有流派殊途同归于同一个架构：

| 层 | 特征 | 谁写 | 何时进上下文 |
|---|---|---|---|
| 核心记忆（curated） | 小、精炼、agent 可自编辑 | agent 自己 | 每次注入（钉在 prompt） |
| 档案记忆（archival） | 大、全、按需检索 | 系统沉淀 | RAG 命中才注入 |

**纯 RAG 不等于记忆**：RAG 是档案层，解决「查资料」；
核心记忆解决「我是谁、我学到什么教训、主人的偏好」——这类高频小知识
走检索不可靠（每次都要碰运气命中），必须钉在 prompt 里。

## 映射到 ru41

现状盘点：

- **档案层已具备**：`knowledge` 服务（fastembed + vault RAG）在
  `worker.rs run_claim` 里对每条消息自动 `retrieval_context()` 注入。
- **核心记忆层缺失**：run_dir 是一次性 tempdir（`ephemeral:true`），
  agent 每次失忆；没有可持久读写的工作区记忆。
- **写入路径已具备**：`ru41-mimo-worker` 容器已挂 `/vault` 和 `/data`，
  agent 天然能读写文件——缺的只是「约定 + 注入」。

落地路径（两步，均不动核心架构大梁）：

1. **核心记忆文件**：vault 里固定一份 `00-Agent-记忆/core-memory.md`
   （容量硬上限，如 4k 字符），worker 在知识注入旁加一个对称的
   记忆注入点，把文件内容前置进每条 prompt；同时用技能包约定
   「值得长期记住的经验（踩坑、偏好、事实修正）追加写入该文件，
   每条一行，满了淘汰最旧的」。vault 本身有 git 历史和 AI 合并，
   记忆被误删可恢复——比裸文件系统方案（Anthropic 要自己做的
   safety）我们白拿。
2. **会话工作区（后续）**：多 agent 底座收敛后，把 run_dir 从一次性
   tempdir 改为每会话持久目录 + 种子 AGENTS.md，承接 Anthropic
   「多会话工程」模式。这动 `worker.rs`/`ports/agent.rs`/codex 适配器，
   等核心组排期。

记忆卫生规则（从各家经验抄）：容量上限强制截断；写入前过
`privacy::Sanitizer`（记忆里不许存凭证）；定期（如每周）由提醒类
任务触发审计，错误条目及时纠——auto-memory 的复利既包括对的也包括错的。
