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

### L2 agent 辅助诊断（等 L1 跑一周数据后再评估）

- 崩溃时自动打包上下文（日志尾 50 行 + 退出码 + RestartCount + 近期变更 commit）
  交给 agent 生成诊断报告推 QQ——**只诊断，不自动修复**。
- 修复动作若要自动化，走审批按钮 + TTL 默认拒绝（复用审批转发底座，
  与「凭据注入审批」同一个闸门模式）。
- 发布后自动回滚：roll.sh 备份已有，缺的只是「发布后 N 分钟 restart 超阈值
  → 切回上一镜像」的判定器。

## 4. 明确不做的事

- **不做 agent 全自动修复生产**：agent 在开发期改挂过东西；自愈闭环必须有人
  的闸门（审批按钮），否则自愈 bug 会变成比故障更大的故障。
- **不逐容器写定制探针**：统一心跳文件方案，探针成本 O(1)。
- **不做宿主级自愈**：宿主异常关机根因未明前，先靠「事件告警」观测，不自动
  动作（重启宿主这种事不能让程序自己决定）。

## 5. 建议执行顺序

1. L-P（compose 资源硬顶 + live `docker update` 立即生效 + watchdog 观测）：一次提交，半天
2. L0 + L1.3（备份）：一次提交，半天
3. L1.1 + L1.2：一个部署批次，一天
4. 跑一周，看告警噪音率，再决定 L2 是否立项
