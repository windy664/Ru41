# 容器故障自愈（研究与设计）

问题：ru41 的容器挂了，怎么让系统自愈、自我修复？agent 在其中扮演什么角色？

## 1. 现状（2026-11-04 实测）

- **进程级拉起（已有）**：全部常驻容器 `restart: unless-stopped`
  （compose 默认，roll.sh 沿用原容器策略）；`restart: no` 仅限
  cron 触发容器（obsidian 索引，每天 03:23 一次性进程，合理）。
- **panic 隔离（部分）**：插件侧过了 `plugin_api::bridge` 的异步入口，
  panic 降级为 Err 不带崩宿主；**宿主侧的 `run_periodic` 轮询没有
  独立 panic 边界**——2026-11-04 class-remind 未过桥的 poll panic
  直接 abort 进程，靠 docker restart 兜底形成一分钟一次的崩溃循环。
- **回滚点（已有）**：`roll.sh` 每次滚动自动备份到 `~/roll-backup-*`。
- **审批机制（已有）**：Interaction 审批转发 + QQ 按钮，agent 的提议
  可以走人审。

缺口：没有故障**检测**（崩溃循环全靠人看 QQ 刷屏或容器不干活才发现，
如 11-03 大湾区论坛漏推）；没有故障**处置**（restart 策略只会原样拉起
坏镜像，panic 循环时反而刷屏）；agent 完全不参与故障响应。

## 2. 分层自愈设计

原则：**越自动的层越确定性**；LLM 只诊断和提议，执行要么是确定性
runbook，要么经人审批。agent 永不持有 docker.sock。

### L0 进程自愈（补强现有，无 LLM）

- `run_periodic` 把每轮 poll 包进独立 `tokio::spawn` + JoinError 处理：
  单轮 panic 记日志、计数、继续下一轮，**进程不再因单次 poll abort**。
  这是 class-remind 事故的直接教训，优先级最高、改动最小。
- 崩溃循环检测本地化：同一进程 5 分钟内 poll 失败（含 panic 降级的
  Err）≥3 次 → 主动把失败报告推为告警（当日去重），而不是默默循环。

### L1 确定性看门狗（新组件，无 LLM）

新增轻量 watchdog（独立小容器或宿主 cron），**唯一持有 docker.sock
的组件**：

- 检测信号：RestartCount 增速、`Up` 后存活时长中位数、日志 panic 特征、
  healthcheck（为每容器补 HEALTHCHECK）。
- 处置 runbook（白名单动作，依次升级）：
  1. 单次异常 → 记录，不动；
  2. 崩溃循环（如 10 分钟内 ≥5 次重启）→ `docker restart` 一次 +
     当日去重告警；
  3. 重启后仍循环 → **自动回滚**到 `~/roll-backup-*` 上一稳定镜像 +
     告警标注「已回滚，待人工确认」；
  4. 回滚仍失败 → `docker stop` 该容器止损（单容器故障不拖垮其余 30
     个）+ 交互审批告警（QQ 按钮确认「停用/再试/我来看」）。
- 关键词：**动作白名单写死在 runbook 里**，看门狗代码路径里不存在
  任意命令执行。

### L2 agent 诊断与修复提议（LLM 提方案，不执行）

看门狗把故障升级为交互审批时，顺手给 agent 建一个诊断任务：

- 输入（只读打包）：出问题容器的日志尾部、最近部署 SHA 与 diff 概要、
  对应插件源码位置、历次告警记录。
- 输出：诊断结论 + 修复建议，三选一形态——
  a) 回滚确认（已由 L1 自动做，人只需核）；
  b) 配置修正（改 ru41.toml 某字段，人审后由看门狗套用）；
  c) 代码修复（生成 patch，走正常 PR + CI，**merge 由人确认**）。
- 约束：agent 任务的产出只是文本与 patch；所有执行动作仍归看门狗
  runbook 或人。agent 在 worker 容器里本就没有 docker 权限，
  这条边界靠挂载隔离天然成立（衔接 privacy-llm-boundary.md 的 P0）。

### L3 预防性（进阶，可选）

- 新镜像滚动后 30 分钟观察窗：期间崩溃循环即自动回滚（把 L1.3 前置）。
- `doctor` 增加在线健康子命令：核对 31 容器 Up、23 插件注册、各 poll
  报告时间戳新鲜度，供看门狗与人工共用。

## 3. 与现有机制的衔接

| 现有机制 | 在自愈里的角色 |
|---|---|
| `run_periodic` + bridge | L0 的 panic 隔落地基 |
| records 告警当日去重（teachermate_alert 模式） | 所有层级告警的统一去重 |
| Interaction 审批 + QQ 按钮 | L1.4 的人审入口、L2 修复建议的确认入口 |
| `roll.sh` 备份 + `roll-backup-*` | L1.3 回滚的现成回滚点 |
| worker 无 docker 挂载 | L2 的权限边界天然成立 |

## 4. 落地顺序建议

1. **L0**（`run_periodic` panic 隔离 + 失败计数告警）：一个核心 PR，
   先做，直接消灭「崩溃循环刷屏」这一类事故。
2. **L1**（watchdog + runbook）：一个独立批次；先只做检测+告警，
   runbook 逐级放开。
3. **L2**（agent 诊断任务）：复用审批链，改动小，跟在 L1 后。
4. **L3** 观察实际故障频率再定。
