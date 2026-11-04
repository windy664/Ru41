# 敏感信息与 LLM 的边界（研究与设计）

问题：哪些敏感信息不该交给 LLM？哪些是任务必需、必须交给的怎么处理？
以及——vault 里有明文密码，agent 底座会不会自己发现？

## 1. 现状盘点（2026-11-04 实测代码与生产配置）

### 已有防线

- **prompt 脱敏**：`src/privacy/` 的 `RandomSanitizer`，5 条正则规则
  （身份证/手机号/邮箱/银行卡/姓名），命中替换为随机同类假值；
  映射表**只存宿主内存**，仅对 `result.summary` 回填（`runtime/worker.rs:221/337`）。
- **技能约定**：`obsidian-organize` SKILL.md 要求 agent「只处理列出的文件，
  不翻阅其他目录，不访问网络」。

### 三个缺口（按严重度）

1. **L2 自主读盘完全不经过脱敏（主风险）**。脱敏只作用于 `task.prompt` 的
   文本。而 worker 容器以 `--dangerously-bypass-approvals-and-sandbox`
   （`codex/exec.rs:167`）+ `sandbox: "danger-full-access"` 运行，且挂载了：
   - `/vault` = 整个 Obsidian Vault（含明文密码笔记）
   - `/etc/ru41` = 整个部署目录，**含 ru41.toml 全部站点账密**
   - `/data` = cookies / openid / 各平台 session 文件

   agent 想读任何一个都读得到，读到的内容**原样进入上游 LLM 上下文**
   （GLM/MiMo 远端 API）。SKILL.md 的「不翻其他目录」是文本约定，
   不是强制边界。

2. **规则覆盖面窄**。密码/口令/token/API key/cookie/学号/住址全不在
   5 条规则内；`password: xxx`、`Bearer eyJ...` 这类形态完全放行。

3. **输出侧只有 summary 回填**。agent 读取密码后写进 vault 的整理产物、
   或塞进中间文件的内容，不经过任何出站扫描。

### 直接回答：agent 会自己发现密码吗？

**会。** 两层都暴露：vault 笔记里的密码（任务要求读收件箱文件，密码笔记
可能正好在里头）、ru41.toml 里的全部凭证（哪怕任务不相关，full-access
下也能读）。目前唯一挡在上游 LLM 之前的只有 prompt 文本脱敏。

## 2. 威胁模型与必要性分类

上游底座 API 提供商（GLM/MiMo）视为**可信边界之外**：任何红区数据
出境即事故。必要性三色：

| 分级 | 内容 | 策略 |
|---|---|---|
| 红区（永不出境） | 密码/口令、API key、cookie/token、身份证/银行卡全号 | 结构性隔离，不靠脱敏规则兜底 |
| 黄区（以占位符进出） | 手机号、姓名、学号、住址 | 已有：随机假值替换 + 回填；按需扩规则 |
| 绿区（可出境） | 课程内容、公开资料、笔记正文 | 直接交给 LLM |

**判据**：删掉这个信息任务就做不成 → 黄区占位符；否则红区移除。
复盘现有 23 个插件的业务（整理/总结/提醒/检索/监控）：**没有任何场景
需要把明文密码交给 LLM**——「整理」不需要密码内容，「查课表」不需要
密码本身（密码是宿主拿来登录的，不是拿来理解的）。

## 3. 方案路线（按性价比排序）

### P0 结构性隔离（不动 LLM 路径，收益最大）

1. **worker 容器不挂 `/etc/ru41`**。worker 只需要：裁剪后的 config
   （agent/notify/todo 段，sources 段的账密剔除）+ `--db` 指向的 state.db。
   装配时由宿主生成只读的裁剪配置写入独立挂载。ru41.toml 从 agent
   视野消失，一劳永逸。
2. **vault 任务改为「快照收件箱」**。obsidian 整理任务不再让 agent 直读
   /vault 全仓：任务创建时把待整理文件**正文内联进 prompt**（先过 P1
   的密码扫描），agent 输出移动/归档指令，由宿主程序落盘执行。
   这同时消解了 full-access 误翻目录的风险——skill 约定升级为物理边界。
3. **vault 密码笔记治理**：一次性扫描（gitleaks 式正则）列出 vault 内
   疑似密码/密钥的笔记 → 人工迁移到密码管理器或 vault 外；之后 cron
   周扫防回潮。

### P1 规则扩容（`src/privacy/`）

- 新增规则：`password/密码[:=]` 赋值形态、`Bearer/JWT` 形态、
  高熵字符串（可选，误报需调）、学号（上下文限定）。
- 假值生成器补对应类型，保持语法合法（回填无误）。
- 规则挂配置开关（`rule("password", false)` 已支持）。

### P2 出站审计（防线兜底）

- 出站点集中扫描：agent 输出进通知渠道前、vault 提交 diff 前，
  命中红区正则即拦截 + 当日去重告警（复用 records 告警去重模式）。
- 这层防的是 P0/P1 漏网的，不是替代。

### P3 沙箱收窄（依赖底座能力，后续）

- codex 从 `danger-full-access` 收到 `workspace-write` + 显式只读白名单
  目录（run_dir、必要的 /vault 只读）；
- 审批策略按任务分级：整理类只读+受控写，网络类单独授权。
- 依赖 Codex 沙箱能力成熟度，先立为方向不急实现。

## 4. 落地顺序建议

P0.1（worker 摘 /etc/ru41）与 P0.2（快照收件箱）一次部署批次完成；
P0.3 扫描立即可做（一次性任务）；P1 一个批次；P2 一个批次；
P3 观望底座。各项互不阻塞，P0 做完前 P1/P2 已能显著降低暴露面。

## 5. 实施状态（2026-11-04）

- [x] **P1 规则扩容**：`RandomSanitizer` 新增 `secret_kv`（password/token/api_key 等
  赋值形态，中文键含「的密码」边界修正）、`bearer_token`、`jwt`、`student_id` 四条规则；
  假值生成器补 `ApiKey`/`Token` 两类。测试在 `tests/sanitize.rs`。
- [x] **P0.3 密码扫描**：`ru41 vault scan-secrets [--vault <路径>] [--out <报告>]`，
  形态库在 `src/privacy/scan.rs`（secret_kv/bearer/jwt + AKIA/xoxb/ghp_/sk-/私钥头/带凭证
  连接串），报告只含 位置+规则名+脱敏预览。服务器 cron 周扫待部署批次注册。
- [x] **P0.2 快照收件箱**：`organize_prompt` 内联待整理文件脱敏正文快照（单文件 2 万字符、
  总量 6 万字符截断，先过 `redact_secrets`）；Agent 只输出 ```ru41-organize JSON 指令
  （moves/deletes/todos/skipped），宿主经 Vault 落盘（重试轮幂等）、待办入库（同标题去重）。
  Agent 侧 vault 全文与文件系统不可见。SKILL.md 同步改写，需镜像部署生效。
- [x] **P0.1 config redact**：`ru41 config redact --out <路径>` 剔除 `sources` 段生成
  worker 专用配置；compose worker 已改为挂载裁剪配置；生产 worker 容器的一次性重建
  （摘全量 /etc/ru41 与 /vault）在部署批次执行。
- [x] **P2 出站审计**：已实施（通知出口拦截 + vault 出口告警，细节见下）。
- [x] **P3 沙箱收窄**：已实施（配置开关默认 bypass，细节见下）。

### P2 实施细节（2026-11-04 落地，同日重构收拢）

- 审计核心收拢在 `src/privacy/egress.rs`（`audit_outbound`、`signature_of`、
  `alert_once` 当日去重、可接线告警钩子）；渠道层 `channels/audit.rs` 只做
  NotifyChannel 包装，vault 审计复用同一套签名与去重，adapters 不再依赖 channels。
- 告警接线：装配层 `notify_pair` 把默认通知渠道注册为告警钩子，
  命中红区当日首报直接推给用户（此前只落 stderr 无人可见）。
- 通知出口：`AuditedNotifyChannel` 包装全部对外渠道（QQ/飞书，含 media 描述字段），
  发送前过 `scan_text`，命中即占位符替换。
- vault 出口：`adapters/vault.rs` `commit_all` 提交前审计 staged diff
  （`git diff --cached --name-only` 逐文件扫描），**只告警不拦截**——vault 是私有仓，
  拦截会卡死自动闭环；同一签名当日只报一次。

### P3 实施细节（2026-11-04 落地）

- 配置开关 `agents.codex.sandbox`：`bypass`（默认，历史行为零变化）| `workspace-write`。
- `CodexAgent`（codex exec）：bypass 走 `--dangerously-bypass-approvals-and-sandbox`，
  workspace-write 走 `--sandbox workspace-write`（codex exec 非交互，approvals 默认 never）。
- `CodexSession`（app-server）：thread/start 的 `sandbox` 参数在
  `danger-full-access` / `workspace-write` 间切换；越权写/网络命令会触发
  requestApproval，已有交互通道会把它转给用户。
- workspace-write 下 agent 命令默认无网络；需联网的任务要在 codex 配置
  `[sandbox_workspace_write] network_access = true`，属任务级显式授权（对齐 P3 设计）。

### P3+ 出口网关（实验项，未实施）

- 网络层只能「管制」不能「脱敏」：docker internal network + 唯一代理出口可保证
  所有出站流量经过检查点，内容级检查仍需 MITM。
- 业界参照：Hermes 的 iron-proxy（自签 CA + HTTPS_PROXY + 域名白名单 + 默认拒绝
  SSRF 网段，observe/log 起步）；企业/政务侧为 DLP 网关与网闸（协议剥离摆渡）。
- 我们形态：mihomo 已在生产做规则出口，可叠一个 observe-only 审计代理
  （记录出站域名/URL + 命中敏感正则告警，不阻断），验证误报率后再考虑强制。
- 已知残留风险（见 iron-proxy 安全模型）：agent 持 allowlist 域名仍可在请求体里
  夹带数据出站（该路径已被 P2 应用层审计覆盖大半）；裸 socket 绕过代理需网络策略兜底。
