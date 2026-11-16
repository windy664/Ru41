# 校园跑（只读附属）

**一句话**：给已经部署在生产机上的 pace-forge 控制台套一层 ru41 的皮——把它的只读数据
（学期进度、跑步记录、违规状态）接到 QQ 命令、agent skill 和定时任务上。

## 是什么、不是什么

| | |
| :--- | :--- |
| **数据源** | 生产机上的 `pace-forge` 容器（运动世界校园的非官方 Rust 客户端），Web 控制台 `:9999` |
| **本插件做什么** | **只读**：学期进度、跑步记录、违规名单；出 Markdown，给 QQ 命令和 agent skill 用 |
| **本插件不做什么** | **不提交任何跑步记录**（`run` / `at` / `ago` / `rand` 一律不接）、不申诉、不改账号 |

写操作（提交记录）不在 P0 范围内，理由见文末「为什么不接提交」。

## 术语（三个名字别混）

- **运动世界校园** = 平台/应用名；
- **校园跑** = 本插件服务的功能：runMode=1 的记分跑（pace-forge Web 控制台里模式选择器
  标的就是「校园跑」；其 CLI/文档里的旧称是「记分跑」）；
- **自由跑** = runMode=2/3，不计体育成绩，本插件不碰。

## 数据接口（全部只读，实测自 pace-forge 0.1.5）

控制台所有 `/api/*`（除 `/api/health`）都要 `X-Pace-Token` 头（或 `?token=`），
响应统一是 `{"ok":true,"data":{...}}` / `{"ok":false,"error":"..."}`。

| 接口 | 插件用到的字段 | 能回答什么 |
| :--- | :--- | :--- |
| `/api/semester` | `summaryRaw`（总/有效里程、次数）、`goalRaw`（`semesterGoal`/`semesterValid`/`goalMethod`）、`goal`（`targetDis`/`daysLeft`/`inTerm`） | 跑够了没、还差几次、什么时候截止 |
| `/api/records` | `rrid`、`totalDis`/`totalTime`、`validDis`/`validTime`、`startTime`、`complete`、`status`、`appealStatus`、`stepFreqStatus`、`avgStepFreq` | 最近几次跑的公里/配速/有效性/是否被标记 |
| `/api/record/{rrid}` | `speed`（点位配速 min/km）、`selDistance`（规则最小单次里程） | 单条详情（目前 CLI 未暴露，备用） |
| `/api/cheat` | `status`（no_record/flagged/unknown/stale）、`label`、`count`、`reason`、`appeal`、`listCount`、`checkedAtMs` | 账号有没有被标记、要不要申诉 |
| `/api/me` | `loggedIn` | 登录态（**姓名与手机号不回显给用户**） |

### ⚠️ pace-forge 自带 CLI 的 `semester` 输出不可信

它把学期目标打成「学期目标 0.0km / 0 次」——**读错了字段**。真实目标在
`goalRaw.semesterGoal`（当前 = 1 次）、`goalRaw.semesterValid`（已完成 0 次）、
`goal.targetDis`（1.0 km，即规则最小单次里程）。插件一律以 API 字段为准，
单测 `status_shows_goal_count_not_cli_zero` 钉住这一点。

### ⚠️ `unknown` ≠ 清白

违规查询返回 `unknown`（服务端字段缺失/格式异常）或 `stale`（超过 5 分钟）时，
插件会额外加一行警告，skill 也要求转告用户「需要去 App 里核对」——把未知报成清白
比不报更糟。

## 配置

`plugins.d/campusrun/config.toml`（或主配置里的 `[sources.campusrun]`）：

```toml
[campusrun]
base_url = "http://172.22.0.1:9999"   # 宿主在 pace-forge 网络上的网关
token_env = "PACE_FORGE_TOKEN"          # 令牌走环境变量，勿写进仓库
timeout_secs = 15
```

**连不上怎么办**：两个容器不在同一个 compose 网络，**容器名 `pace-forge` 解析不了**。
可用地址（ru41 容器内实测）：

- `http://172.22.0.1:9999`（宿主在 pace-forge 网络上的网关）
- `http://172.17.0.1:9999`（docker0 网关）
- `http://192.168.1.223:9999`（宿主 LAN IP）

想让容器名直连，把 pace-forge 也挂进 `ru41-net` 网络即可（改它的 compose 一行）。

令牌从容器环境变量 `PACE_FORGE_TOKEN` 注入；也可以在 gitignore 的本地
`config.toml` 里写 `token = "..."`（配置里的 token 优先于环境变量）。

## 对外接口

### CLI

```bash
ru41 campusrun status          # 学期进度：有效里程/次数、目标、剩余天数、达标判定
ru41 campusrun records [条数]   # 最近记录表（默认 5）
ru41 campusrun cheat           # 违规名单状态
ru41 campusrun all             # 三样一起（用户泛问「我的校园跑怎么样」）
```

输出是 Markdown。

### QQ 命令

`/校园跑`（别名 `/跑步`、`/campusrun`），参数可选 `进度` / `记录` / `状态` / `全部`；
**只单聊**（`require_private`），参数不认识就不拦、交给 agent。

### 定时任务

`campusrun` 每小时拉一次只读快照，结果落在 `schedule run campusrun` 的输出与容器日志。
**P0 不推 QQ**（每日提醒在下一期接入）。

### Skills

- `campus-run-progress`：进度类问题（跑够了没、还差多少、什么时候截止）。
- `campus-run-records`：记录与账号状态类问题（上次那条多少配速、有没有被标记、要不要申诉）。

两个 skill 都写明：只读、不得编数字、命令报错要照实说。

## 为什么不接提交（run/at/ago/rand）

pace-forge 自己的文档把这些坑列得很清楚：提交后 **600 秒静默窗口**、打卡点接口
**5 分钟限流 3 次**、异常提交**计风控次数**、**单设备登录冲突锁号一小时**。机器人代替
人跑，异常形态最难解释；而且这属于「替用户做决定」，不该由定时任务触发。
P0 只读，操作接口等有明确需求时再单独评估（届时至少要有：用户明确要求 + 回执二次
确认 + 全部留痕）。

## 已知限制

- 依赖 pace-forge 控制台在跑；控制台挂了插件就查不到（错误会直说「连不上」）。
- 打卡点/围栏/有效时段/规则最小里程这些「规则面」P0 没接（`/api/points`、`/api/fences`、
  `/api/starttimes`），要判断「今天能不能过」还得去控制台看。
- 记录详情（`/api/record/{rrid}`）的轨迹字段很大，插件不进 agent 上下文。

## 部署

插件是 cdylib，按 `docs/plugin-development.md` 的规矩编（容器内 `rust:1.98-trixie`），
产物 `plugins/target-plugins/release/libru41_plugin_campusrun.so`，
scp 到宿主 `~/ru41-mimo-smoke/plugins/` 后重启需要它的容器（QQ 命令在 `ru41-qq`）。
