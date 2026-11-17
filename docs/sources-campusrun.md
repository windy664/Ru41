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

### ⚠️ 学期目标是「里程」，不是次数

服务端 `semesterGoal` = **目标里程（米）**、`semesterValid` = 已完成里程（米）、
`goalMethod=2` = 按里程；目标次数在 `targetCount`（0 = 不计次）。
（pace-forge 源码 `api/semester.rs` 的 `parse_goal` 写得很明确；它自带的 CLI
`semester` 把目标打成「0.0km / 0 次」，读错字段了，**插件一律以 API 为准**。）

另外两件事容易混：

- **学期目标**（服务端给的学期指标，当前 ≈ 无门槛）；
- **单次门槛**：`minDistance=1000`（单次至少 1.00 km）、`minRunTime`（至少 5 分钟）——
  这是「这条能不能提交」的线。

### ⚠️ `unknown` ≠ 清白；`listCount` ≠ 你的违规数

服务端原始响应（`POST /api/v78/cheat/cheatlist`）里是**两回事**：

- `self` = **你自己的**违规信息（`null` = 干净）；
- `list` = **全校违规通报榜**（别人。2026-11-17 实测 9 人，含姓名/uid/次数/校区）。

控制台把 `self` 压成 `status`/`label`/`count`/`reason`/`appeal`，把 `list` 压成
`listCount`（人数）。插件渲染时必须分开说：违规查询返回 `unknown`（服务端字段缺失）
或 `stale`（超过 5 分钟）时**不等于清白**，要提示去 App 核对；`listCount` 要标成
「全校通报榜 N 人，与你无关」，别让人读成「你被通报了 N 次」。

## 配置

**插件专属配置目录**：`plugins.d/campusrun/config.toml`（顶层段名就是插件的配置键名）：

```toml
[campusrun]
base_url = "http://172.20.0.1:9999"   # 宿主在 ru41-net 上的网关
token_env = "PACE_FORGE_TOKEN"          # 令牌走环境变量，勿写进仓库
timeout_secs = 15
allow_submit = false                    # 是否允许真提交；默认关
# pace_min = 5.5                       # 配速区间（分/km），默认 5.5~6.6
# pace_max = 6.6
alt_min = 15                           # 海拔区间（米）
alt_max = 45
# silence_secs = 600                   # 提交后的静默窗口
# confirm_ttl_secs = 900               # 待确认有效期
```

> **门槛不在配置里**：距离/时长/配速/点位的判定都在服务端（`policy.minDistance`、
> pace-forge 的 preflight、记录详情的 `reasonList`），各门课要求不同，写死会误判。

> **「什么时候跑」也不在配置里**：**由 agent 从对话里读出时态**，再按请求传参
> （不带时间参数 = 刚跑完；`--ago 30` = 半小时前那次；`--at 09:00 --days 1` = 昨天九点；
> `--rand --days 2` = 前天某个随机时刻）。
> 插件只把意图翻译成 `GET /api/starttimes?mode=…&days=…&dur=…`，**起点由服务端算**
> （控制台的 `resolveStartAt()` 走的就是这一步），拿回 `startMs` 直接用。
> 服务端还会照自己的 limits 把时段**收窄**（`clamped`）——越界（往前补 5 天）
> 也是服务端报错，插件只转述，不自己那套「在随机小时窗口里挑一个整点」的规则。
> 未来时间（「半小时后」）工具不支持（记录只能补已发生的：开始+用时 ≤ 现在，最多往前 3 天），
> 要真到点再跑，用宿主的一次性提醒（`ru41 todo add --remind "YYYY-MM-DD HH:MM"`）。
> 模板全文见 `plugins/campusrun/config.example.toml`。

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
- **`ru41-mimo-worker` 也要挂 `plugins.d`**（`compose.yaml` 已补：
  `-v ../plugins.d:/etc/ru41/plugins.d:ro`），否则 worker 里的插件配置读不到。

后果很隐蔽：QQ 命令正常，**agent 在 worker 里跑 `ru41 campusrun status` 却报
「没有 pace-forge 控制台令牌」**——2026-11-17 用户在 Telegram 问「我校园跑跑了多少
公里」没得到答案，根因就是这个（agent 甚至自己翻了 8 条托管凭据去试令牌）。

修法：给 worker 容器补一个挂载 `-v ~/ru41-mimo-smoke/plugins.d:/etc/ru41/plugins.d:ro`
（**已于 2026-11-17 落地并验证**：worker 里 `ru41 campusrun status` 与
`ru41 jwxt classes` 均能正常读到插件配置）。
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
ru41 campusrun rules           # 规则面：单次门槛、补跑限制、必过点、围栏
ru41 campusrun plan --laps 3   # 预演（生成轨迹 + 八类自检，**不提交**）
ru41 campusrun plan --km 1.2   # 预演（不提交）
ru41 campusrun submit --laps 3  # 登记一条待确认的提交（**仍不提交**），返回确认口令
ru41 campusrun confirm <token> # 确认并提交（**写操作**）
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

## 跑一次（换算 → 预演 → 确认 → 提交）

### 参数换算（插件做，别让模型做算术）

| 参数 | 值 | 换算 |
| :--- | :--- | :--- |
| 跑法 | 记分跑 | `runMode=1`、`sportType=1`（固定） |
| 路线 | 自动规划 | `mode=free` + 经纬度留空 → 工具 planner 自己选（实测 `trackMode=stadium`，绕田径场） |
| 距离 | 「N 圈」→ N×单圈 400.0 m（第 1 道）；「X 公里」→ 直接用 | 单圈 = `2×84.39 + 2π×(36.8 + 1.22×(道次-1))`（第 1 道 400.0、第 2 道 407.7、第 4 道 423.0） |
| 配速 | 5.5~6.6 分/km | **用户给的推荐参数**，原样用（不夹范围）；时长 = 距离 × 区间里**随机**取的一个配速 |
| 时长 | 自动 | 照抄控制台 `autoDur(dist)`：`Math.max(3, dist × 随机配速)`；服务端自己的默认是**固定 720 秒**、不按距离算 |
| 步数 | 自动 | 工具的步频模型合成 |
| 何时跑 | **按次请求给**（agent 从对话里读） | 不带时间参数 = 刚跑完；`--ago 30` = 半小时前；`--at 09:00 --days 1` = 昨天九点；`--rand --days 2` = 前天随机。**起点由服务端算**：`GET /api/starttimes?mode=…&days=…&dur=<秒>` → 拿回 `startMs`（最多往前 3 天） |
| 经纬度 | 留空 = 用围栏 | 不传 `locBody` → 工具取围栏中心 |
| 海拔 | 15~45 m | `altMin`/`altMax`（写死 150±8 会在高原场景露馅） |

**门槛以服务端为准，插件不自己写死**（2026-11-16 改，2026-11-17 再收紧）：
- **插件只做两件事**：驱动 Web 端那套操作（同样的 `/api/plan` `/api/run` 请求体）、
  套上用户给的推荐参数。**它不自己判能不能跑**——Web 端也没有距离这道闸
  （实测 0.9 km、配速合法，工具照样「全部通过」放行），能不能跑由工具自己的
  preflight 说了算，插件原样转述；
- **时长**：控制台的 `autoDur(dist)` 是**浏览器端 JS**（区间里随机取配速、下限 3 分钟），
  服务端自己的默认时长是**固定 720 秒**（实测 1.2/2.0/3.0 km 全是 12.0 分钟）、
  **不按距离算**——所以插件这一层必然要算，但必须**照抄控制台的算法**，
  不自创变体（之前这里取的是区间中值，控制台是随机的）。
  插件**不额外抬时长**（曾有个「抬到服务端要求的分钟数」的开关，已删）：
  服务端「跑N分钟以上」那条规则只在「上次提交的判定」里原样转述，
  能不能过由服务端判——和 Web 端完全一致；
- **配速**是用户给的推荐参数，原样用（不夹范围、不回落默认）；
- **距离门槛**只在输出里**转述**（`服务端最小里程：1.00 km`），不拿它当闸门；
- **时长/配速/点位**由 pace-forge 自己的 preflight 判，结论原样转述：
  预演失败时服务端只给一句「自检未通过：1 致命 / 0 警告」，**具体哪条致命/警告在
  `logs` 里**（`✗ 致命 [CP_MISS] …`、`! 警告 [PACE_FAST] …`）——插件把工具的原话带上，
  不自己重新判一遍；
- **上次提交的逐条规则判定**从记录详情的 `reasonList` 读（门槛各门课不同：
  实测有「最少1.00公里 / 跑7分钟以上」，也有「最少1.50公里 / 跑6分钟以上」）。

### 闸（缺一不可）

1. **意图闸**：只在用户明确要求时；定时任务永不提交。
2. **工具预演闸**：`POST /api/plan` 的 `data.ok` 为 false → 停在预演，原样转述工具给的原因
   （连工具自己的致命/警告行一起转述）。插件**不另设**距离/配速判定。
3. **确认闸**：`submit` 只登记待确认（返回口令），`confirm <口令>` 才提交；口令有有效期。
4. **静默窗口**：上一次提交后 `silence_secs`（默认 600）秒内拒绝任何跑步请求。
   **「上次提交」的时刻读服务端的 `lastSubmitMs`（`/api/accounts`）**——从 Web 控制台
   交的那次也算数，插件不自己另记一本台账；本地只留一条兜底记录。
   间隔多长是插件配的保守值：**服务端的间隔阈值没有在 API 里暴露**。
5. **台账三态**：`planned → submitting → accepted(rrid) / rejected / failed / unknown`。
   网络类错误 = **结果未知**（服务端可能已入账）→ 标 `unknown`，**不可重试**：
   同一时间段跑两次物理上不可能，最容易被判异常。
6. **不接 `/api/batch`**：一次学期交多条记分跑是最可疑的形态。
7. **默认关闭**：`allow_submit=false`，不开就只做预演。

工具自带的闸（走 `POST /api/run` 会继承，因为 Web API 与 CLI 走同一条 flow）：
八类 preflight 自检 → 报文派生字段自检 → 轨迹段自检 → 静默窗口 → 工具自己的台账。

## 提交的风险面（为什么这么多闸）

pace-forge 自己的文档把这些坑列得很清楚：提交后 **600 秒静默窗口**、打卡点接口
**5 分钟限流 3 次**、异常提交**计风控次数**、**单设备登录冲突锁号一小时**。机器人代替
人跑，异常形态最难解释；而且这属于「替用户做决定」，不该由定时任务触发。
所以提交路径默认关着（`allow_submit=false`），且必须用户明确要求 + 二次确认 + 全部留痕。

## 踩过的坑

- **`ru41 <模块> run` 会被宿主抢走**：宿主把 `run` 当成 tracker 的长驻任务（`poll|run`
  归宿主分发），插件的 cli 钩子根本收不到 → 提交子命令叫 `submit`，不叫 `run`。
  （2026-11-17 上线时踩到：`ru41 campusrun run --laps 3` 跑成了 tracker 的常驻任务。）
- **预演每次的轨迹都不一样**：`/api/plan` 每次重新生成（随机种子），
  所以「预演通过」不等于「提交的那条也长这样」——好在 `/api/run` 自己会再跑一遍
  preflight，自检不过它自己就不提交。

## 已知限制

- 依赖 pace-forge 控制台在跑；控制台挂了插件就查不到（错误会直说「连不上」）。
- 规则面（必过点/围栏/补跑窗口）走 `rules` 与 `plan` 拿；`plan` 会消耗打卡点接口配额
  （服务端限 5 分钟 3 次），别高频连着调。
- 记录详情（`/api/record/{rrid}`）的轨迹字段很大，插件不进 agent 上下文。
- 提交结果**未知**（网络断了、服务端可能已入账）时，插件标 `unknown` 且不可重试，
  需要去运动记录页核对——这一步只能人做。
- 插件看不到 pace-forge 自己的提交台账（它在宿主的数据目录里），所以插件自建一份
  `campusrun.submit` 记录当台账。

## 部署

插件是 cdylib，按 `docs/plugin-development.md` 的规矩编（容器内 `rust:1.98-trixie`），
产物 `plugins/target-plugins/release/libru41_plugin_campusrun.so`，
scp 到宿主 `~/ru41-mimo-smoke/plugins/` 后重启需要它的容器（QQ 命令在 `ru41-qq`）。
