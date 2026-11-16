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

**插件专属配置目录**：`plugins.d/campusrun/config.toml`（顶层段名就是插件的配置键名）：

```toml
[campusrun]
base_url = "http://172.20.0.1:9999"   # 宿主在 ru41-net 上的网关
token_env = "PACE_FORGE_TOKEN"          # 令牌走环境变量，勿写进仓库
timeout_secs = 15
```

生产实况：宿主 `~/ru41-mimo-smoke/plugins.d/campusrun/config.toml`（0600，
令牌内联）。**不要塞进主 toml**——worker 与其他容器用的不是同一份配置文件，
塞主 toml 两边就对不上（见下面的「worker 要单独补挂载」）。

**连不上怎么办**：两个容器不在同一个 compose 网络，**容器名 `pace-forge` 解析不了**。
可用地址（ru41 容器内实测）：

- `http://172.22.0.1:9999`（宿主在 pace-forge 网络上的网关）
- `http://172.17.0.1:9999`（docker0 网关）
- `http://192.168.1.223:9999`（宿主 LAN IP）

想让容器名直连，把 pace-forge 也挂进 `ru41-net` 网络即可（改它的 compose 一行）。

令牌两种给法：容器环境变量 `PACE_FORGE_TOKEN`，或配置里内联 `token = "..."`
（配置里的 token 优先于环境变量）。生产上用的是**内联**——令牌在 pace-forge 的
compose 里本来就是明文，再抄一份到 gitignore 的 `plugins.d/campusrun/config.toml`
（0600）不增加暴露面。

### ⚠️ worker 容器要单独补一个挂载（2026-11-17 踩过）

插件配置的根 = **主配置文件同级的 `plugins.d/`**（`Config::load` 启动时合并）。
生产上：

- 绝大多数容器把整个 `~/ru41-mimo-smoke` 挂在 `/etc/ru41` → 天然能读到
  `/etc/ru41/plugins.d/<插件>/config.toml`；
- **`ru41-mimo-worker` 只挂了 `ru41-worker.toml` 一个文件**（它和 `ru41.toml` 是两份
  配置，worker 读前者）→ 容器里没有 `/etc/ru41/plugins.d`，插件配置**读不到**。

后果很隐蔽：QQ 命令正常，**agent 在 worker 里跑 `ru41 campusrun status` 却报
「没有 pace-forge 控制台令牌」**——2026-11-17 用户在 Telegram 问「我校园跑跑了多少
公里」没得到答案，根因就是这个（agent 甚至自己翻了 8 条托管凭据去试令牌）。

修法：给 worker 容器补一个挂载 `-v ~/ru41-mimo-smoke/plugins.d:/etc/ru41/plugins.d:ro`。
重建容器照抄 `deploy/roll.sh` 的「从 docker inspect 逐字段恢复」逻辑（env/挂载/网络/
重启策略都保留）；roll.sh 之后也会照原样带上这个挂载。

**教训**：插件配置放 `plugins.d/`，别塞进主 toml——主 toml 每个容器一份
（worker 与其他容器用的还不是同一份），塞主 toml 必然两边不一致。

## 对外接口

### CLI

```bash
ru41 campusrun status          # 学期进度：有效里程/次数、目标、剩余天数、达标判定
ru41 campusrun records [条数]   # 最近记录表（默认 5）
ru41 campusrun cheat           # 违规名单状态
ru41 campusrun all             # 三样一起（用户泛问「我的校园跑怎么样」）
```

输出是一个 JSON 字符串，里面装的是 Markdown（`\n` 是换行）——和 jwxt 一样，
宿主统一把 CLI 结果 `to_string_pretty` 打出来。

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
