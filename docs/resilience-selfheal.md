# 方向二评估：容器故障自愈（resilience / self-heal）

> 2026-11-04 评估。结论先行：现有兜底（docker 重启策略 + 插件 panic 隔离）解决了
> 「崩了能拉起来」，但完全没有解决「崩了没人知道」和「不退出但已废」——
> 建议顺序 L-P（预防）→ L0（小）→ L1（主）→ L2（观望）。

## 0. 预防层 L-P：自愈解决不了的那一类（必须事前防）

自愈全是「事后反应」：die 事件告警再快也在进程退出之后，心跳探测僵尸
循环最快也要等一个探测周期——期间 CPU 早已烧满。而生产机是 8 线程笔记本
（i5-8250U / 7.1G），热保护关机是瞬时物理事件，**无法自愈，只能预防**：

1. **容器资源硬顶**：compose 给全部服务加 `cpus`/`mem_limit`（通用 0.5 核 /
   256m；worker、browser 这类吃 CPU 的 2.5 核；ASR sidecar 单独评估）。
   上限的意义是物理保证：任何容器跑飞最多吃自己那份，整机负载封顶，
   热关机失去触发条件。这不是探测，是让故障「够不着」关机阈值。
2. **内存同理**：当前 34 个容器无 mem_limit，内核 OOM-killer 在无约束时
   可能杀错受害者；有硬顶后 OOM 只会杀掉越界的那个。
3. **宿主 watchdog（observe-only 先行）**：load1 持续超阈值时记录 offender
   快照（docker stats top + 容器名）到日志，先观测一周确认阈值，再决定
   是否加自动暂停动作。QQ 告警接入 L1 的通知通道。

## 1. 故障面清单（按真实发生过的事件）

| 故障类型 | 真实先例 | 现有保护 | 缺口 |
|---|---|---|---|
| 进程 panic 退出 | egress-audit SIGSEGV crash loop（今天，RestartCount 20） | restart=unless-stopped 自动拉起 | 拉起后继续崩，**无人知晓**；健康检查 none |
| 轮询 panic（插件） | 今日 bridge 隔离验证 | `ru41_plugin_api::bridge` catch_unwind ✓ | — |
| 轮询 panic（内置任务） | 无已知案例，但 main.rs 内置闭包（todo-remind/class-remind）无隔离 | 无 | panic 带崩整个容器进程，该容器全部任务停摆 |
| 死循环/资源耗尽（不退出） | obsidian 紧密循环烧到笔记本热保护 | interval 30s 下限（防配置错误型）；34 容器均无 cpus/mem_limit | 逻辑型死循环无探测；restart 管不了「活着但已废」；**热关机只能靠 L-P 硬顶预防** |
| 静默失败 | 「总是任务失败又不给原因」「今天怎么没推送」（多次） | run_periodic Err 只落 stderr 等下轮 | **无失败升级告警**——当前最痛的一层 |
| 数据丢失 | 宿主两次异常关机（软件问题，根因在查） | `deploy/backup.sh`（sqlite .backup）存在 | **服务器 cron 没排备份任务**，state.db 零备份 |
| 发布事故 | Dockerfile 缺插件、镜像缺 so（CI 照绿） | roll.sh inspect 全量备份可回滚 ✓ | 无「发布后自动判定异常→回滚」 |
| 宿主宕机 | 两次异常关机 | docker systemd enabled + unless-stopped → 上电自恢复 ✓ | 宿主自身健康无人看（容器层自愈管不到宿主） |
| 外部依赖故障 | 学校接口抽风、QQ 网关波动 | 失败等下轮重试 | 连续失败无升级，用户只能靠「没收到推送」反推 |

## 2. 现有保护盘点（动工前必须知道的「已经有了」）

- 全部 ru41 容器 `restart=unless-stopped`；docker systemd enabled → 宿主断电/重启后全家自恢复
- 插件异步入口统一过 bridge（catch_unwind + 独立 runtime）→ 插件 poll panic 不带崩进程
- `run_periodic`：失败记日志等下轮；间隔下限 30s 防紧密循环
- Interaction TTL 过期即拒，无无限阻塞
- roll.sh 从 inspect 逐字段重建，发布可回滚（备份在 `~/roll-backup-*`）
- Cargo 无 panic=abort（默认 unwind）→ catch_unwind 技术前提成立

## 3. 分层方案

### L0 代码级隔离（约半天）

- `run_periodic` 的 `poll().await` 包 `AssertUnwindSafe + catch_unwind`：
  内置任务 panic 降级为当轮失败，进程不死，其余任务继续。
- 同时加**连续失败计数**：同一任务连续 N（建议 5）次 panic/Err → 走通知渠道
  告警一次（当日去重），消息带最近一次错误原文——直接消灭
  「任务失败又不给原因」。

### L1 探测与告警（1–2 天，ROI 最高层）

1. **容器死亡告警**：宿主 systemd 服务订阅 `docker events --filter event=die`，
   die 事件推 QQ（容器名 + 退出码 + 是否被 restart policy 拉起）。崩溃从
   「事后发现没推送」变成「当场一条消息」。
2. **心跳 + 健康检查**：`run_periodic` 每轮 touch `/data/heartbeat/<任务名>`；
   各容器 HEALTHCHECK 检查心跳新鲜度（阈值 = 3×任务间隔）；配 autoheal
   （或 systemd timer 扫 `docker ps --filter health=unhealthy`）自动重启
   ——覆盖「不退出但已废」的死循环型故障。
3. **state.db 定期备份**：把已有的 `deploy/backup.sh` 排进 cron（每日 04:21），
   保留 7 份滚动。宿主已经异常关机过两次，这是当前数据面最大的裸奔点。

### L2 agent 自愈：issue + PR 闭环（等 L1 跑一周数据后再评估）

> 2026-11-04 修订：原方案「agent 出诊断报告 + 审批按钮收 patch」升级为
> 「agent 直接对 GitHub 仓库开 issue + PR」。调研结论：这就是业界已验证的
> 路径（Sentry Seer 的 Autofix：错误→根因分析→自动开 PR→CI+人工 review 把关），
> 而 PR 相比自造审批按钮白送三层：CI 门禁、可视化 review 交互、git 审计记录。
> ru41 全链路已齐活（开源仓库 + CI 出镜像 + roll.sh 部署），缺的只是接线。

- 触发链：容器 die / 连续失败（L1 探测）→ 自动打包上下文（日志尾 50 行 +
  退出码 + RestartCount + 近期变更 commit）→ agent（codex）根因分析 →
  开 GitHub issue（诊断报告）→ 能定位的开分支 + patch + PR →
  你 merge → roll.sh 部署闭环。
- 学界背书与验收范式（SWE-bench / SWE-agent，普林斯顿）：agent 解决真实
  GitHub issue 的通过标准不是「编译过」，而是「fail-to-pass 测试转绿 +
  原有测试不回归」；SWE-bench Verified 当前解决率 ~80%——剩下 20% 就是
  merge 权必须留在人手里的理由。
  - SWE-agent 的核心贡献 ACI（Agent-Computer Interface）：agent 表现取决于
    给它的工具接口，与模型同等重要——仓库的 AGENTS.md、技能包、ru41 CLI
    子命令就是我们自己的 ACI，持续打磨。
- 立项前置条件（依赖，不是可选项）：
  1. **L1 先行**：PR 修不了正在 crash loop 的容器（延迟分钟级），
     止血靠 restart/autoheal，PR 负责「别再犯」。
  2. **CI 升级为 test-gate**：现在 CI 只验编译，而类型安全的死循环照样过
     编译（obsidian 烧机先例）。PR 上必须跑 cargo test + 容器冒烟
     （起容器验证 poll 一轮成功），对齐 SWE-bench 验收标准。
  3. 降噪：同一根因去重，避免 issue/PR 刷屏。

## 4. 明确不做的事

- **不做 agent 全自动修复生产**：编译器只担保类型与内存安全，不担保行为
  正确——本仓库全部生产事故（obsidian 烧机、egress-audit/ClassRemind
  未过桥、Dockerfile 缺插件）都是编译通过、CI 全绿状态下发生的。
  修复的不可逆决策（merge、回滚）必须人在环：PR review 就是那个闸门。
- **不逐容器写定制探针**：统一心跳文件方案，探针成本 O(1)。
- **不做宿主级自愈**：宿主异常关机根因未明前，先靠「事件告警」观测，不自动
  动作（重启宿主这种事不能让程序自己决定）。

## 5. 建议执行顺序

1. L-P（compose 资源硬顶 + live `docker update` 立即生效 + watchdog 观测）：一次提交，半天
2. L0 + L1.3（备份）：一次提交，半天
3. L1.1 + L1.2：一个部署批次，一天
4. 跑一周，看告警噪音率，再决定 L2 是否立项
