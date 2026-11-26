# 校园跑（pace-forge 的 ru41 皮）

**一句话**：给已经部署在生产机上的 pace-forge 控制台套一层 ru41 的皮——把它的
**只读数据**（学期进度、跑步记录、违规状态）接到 QQ 命令、agent skill 和定时任务上，
外加**两道写操作**：用户明确要求时提交一条跑步记录、提交后自动复查有没有被回扫判违规。

## 是什么、不是什么

| | |
| :--- | :--- |
| **数据源** | 生产机上的 `pace-forge` 容器（运动世界校园的非官方 Rust 客户端），Web 控制台 `:9999` |
| **本插件做什么** | **读**：学期进度、跑步记录、违规名单；出 Markdown，给 QQ 命令和 agent skill 用<br>**写（受限，2026-11 起）**：`submit` 登记待确认 → `confirm <口令>` 才真提交；提交后第 3 天自动复查有没有被回扫判违规 |
| **本插件不做什么** | **不自动提交**（定时任务永不提交）；不申诉、不改账号；**不替服务端解释**为什么判违规 |

写路径有七道闸（见「闸（缺一不可）」），`allow_submit=false`（缺省）时 `submit`/`confirm`
直接被拒，**但 `plan` 预演照常可用**——预演是只读的。

> 「不自动提交」是硬约束：`ru41-campusrun` 常驻容器跑的是 `schedule run campusrun`，
> 它的 tracker **只读**（拉进度/记录/违规状态）+ **事后复查**，
> **不会自己提交跑步**。提交只在你明确下命令时走。

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
track_mode = "stadium"                  # 轨迹模式：stadium（真机跑道）/ free（围栏内自由规划）；缺省 stadium
# pace_min = 5.9                       # 配速区间（分/km），默认 5.5~6.6；生产用 5.9 压住「跑7分钟以上」
# pace_max = 6.6
alt_min = 15                           # 海拔区间（米）
alt_max = 45
# silence_secs = 600                   # 提交后的静默窗口
# confirm_ttl_secs = 900               # 待确认有效期
```

> `track_mode` 缺省 `stadium`（2026-11-26 起）：`stadium` 让 pace-forge 走
> **真机跑道轨迹**（绕田径场），跑不成时它内部自己回退围栏内规划（`stadium_fallback`）。
> 这个选择正是踩到上游 issue #37 的地方 —— 见下面「外框太圆被误当跑道」。

> **门槛不在配置里**：距离/时长/配速/点位的判定都在服务端（`policy.minDistance`、
> pace-forge 的 preflight、记录详情的 `reasonList`），各门课要求不同，写死会误判。

> **「什么时候跑」也不在配置里**：**由 agent 从对话里读出时态**，再按请求传参
> （不带时间参数 = **当天随机、几天前 0**，用户推荐参数里的默认；
> `--ago 30` = 半小时前那次；`--at 09:00 --days 1` = 昨天九点；
> `--rand --days 2` = 前天某个随机时刻；`--now` = 就当刚跑完）。
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

> **改配置要重启**：插件的 `config` 是**启动时快照**（`qq_handlers` / `tracker` / CLI
> 都在注册时就把 `CampusrunSection` 读进内存了），改完 `plugins.d/campusrun/config.toml`
> 必须重启加载该插件的容器才生效（`ru41-qq`、`ru41-mimo-worker`、`ru41-telegram`）。

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

`submit` 只登记（返回 6 位口令），`confirm <口令>` 才真提交——两步之间是给人看的窗口。
`allow_submit=false` 时 `submit` 直接被拒；`plan` 不受这个开关影响（只读）。

输出是一个 JSON 字符串，里面装的是 Markdown（`\n` 是换行）——和 jwxt 一样，
宿主统一把 CLI 结果 `to_string_pretty` 打出来。

### QQ 命令

`/校园跑`（别名 `/跑步`、`/campusrun`），参数可选 `进度` / `记录` / `状态` / `全部`；
**只单聊**（`require_private`），参数不认识就不拦、交给 agent。

### 定时任务

`campusrun` 每小时拉一次只读快照，结果落在 `schedule run campusrun` 的输出与容器日志。

**QQ 只在一种情况下会被推**：提交后第 3 天的**事后复查**发现那条被服务端判了违规
（见「提交后自动事后复查」）。学期进度这类常规快照**不推**（每日提醒还没接）。

### Skills

- `campus-run-progress`：进度类问题（跑够了没、还差多少、什么时候截止）。
- `campus-run-records`：记录与账号状态类问题（上次那条多少配速、有没有被标记、要不要申诉）。

两个 skill 都写明：只读、不得编数字、命令报错要照实说。

### 提交路径不靠 skill

`submit`/`confirm` 只走 CLI（`ru41 campusrun submit` → 6 位口令 → `confirm`），
**不开放给 agent**：agent 拿到「口令」没有意义（用户要亲手敲），
而且提交属于「替用户做决定」，必须用户明确下令。

## 跑一次（换算 → 预演 → 确认 → 提交）

### 参数换算（插件做，别让模型做算术）

| 参数 | 值 | 换算 |
| :--- | :--- | :--- |
| 跑法 | 记分跑 | `runMode=1`、`sportType=1`（固定） |
| 路线 | 跑道轨迹 | `mode=stadium`（配置项 `track_mode`，缺省 stadium）→ 绕真机跑道；跑不成时 pace-forge 内部回退围栏内规划 |
| 距离 | 「N 圈」→ N×单圈 400.0 m（第 1 道）；「X 公里」→ 直接用 | 单圈 = `2×84.39 + 2π×(36.8 + 1.22×(道次-1))`（第 1 道 400.0、第 2 道 407.7、第 4 道 423.0） |
| 配速 | 5.5~6.6 分/km | **用户给的推荐参数**，原样用（不夹范围）；时长 = 距离 × 区间里**随机**取的一个配速 |
| 时长 | 自动 | 照抄控制台 `autoDur(dist)`：`Math.max(3, dist × 随机配速)`；服务端自己的默认是**固定 720 秒**、不按距离算 |
| 步数 | 自动 | 工具的步频模型合成 |
| 何时跑 | **按次请求给**（agent 从对话里读） | 不带时间参数 = **当天随机、几天前 0**（用户推荐参数的默认）；`--ago 30` = 半小时前；`--at 09:00 --days 1` = 昨天九点；`--rand --days 2` = 前天随机；`--now` = 刚跑完。**起点由服务端算**：`GET /api/starttimes?mode=…&days=…&dur=<秒>` → 拿回 `startMs`（最多往前 3 天） |
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

### ⚠️ 「跑N分钟以上」是学校判分时查的，pace-forge 的 preflight 不查（2026-11-18 实测）

上面说「能不能过由服务端判」——**对时长这一条要打个补丁**：查 preflight 的那条闸管不到时长。

实测（`POST /api/plan`，1.2 km）：

| 时长 | preflight 结论 |
| :--- | :--- |
| 6.6 分钟 | 全部通过 |
| 7.08 分钟 | 全部通过 |
| 7.5 分钟 | 全部通过 |

`/api/plan` 的 `policy` 里 `minDistance=1000`、`validTime=0`——**它根本不知道任务要几分钟**。
时长是**学校在判分时**拿 `reasonList` 里的「跑7分钟以上」核的，而
`reasonList` 只在**已提交的记录详情**里才有（`/api/record/{rrid}`）。

拿已提交的记录对齐就很清楚了（7 条记录，时长 vs `complete`）：

| 时长 | 距离 | complete | 没过的那条 |
| :--- | :--- | :--- | :--- |
| 426 s | 1212 m | ✅ | — |
| **399 s** | 1216 m | ❌ | 「最少要跑1.50公里」（**距离**不够，不是时长） |
| 505 s | 1190 m | ✅ | — |
| 498 s | 1249 m | ✅ | — |
| 642 s | 1529 m | ✅ | — |
| 475 s | 1115 m | ✅ | — |
| 39 s | 32 m | ❌ | 时长、距离都不够 |

门槛正好卡在 399 s 和 426 s 之间 = **420 s（7 分钟）**。

**所以配速下限必须自己压住任务的最短时长**（这是插件唯一能调的杠杆，CLI 只认
`--km`/`--laps`，时长恒等于「距离 × 随机配速」）：

```
pace_min ≥ 任务要求的最短分钟数 ÷ 距离
```

1.2 km 要过 7 分钟 → `pace_min ≥ 5.83` → **取 5.9**（时长下限 1.2×5.9 = 7.08 分钟 = 425 秒）。
默认 5.5 时时长下限 6.6 分钟 = 396 秒，**约 30% 的概率**（pace 落在 [5.5, 5.83)）会被学校判不合格。

配速本身还有一道学校闸：「配速控制在2′10″-10′00″」——两个窗口都要落在里面。
**服务端规则按任务变**（见过 5/6/7 分钟、1.00/1.50 公里），换任务要跟着改 `pace_min`。
改完记得重启（配置是启动时快照，见下）。

### 外框「太圆」被误当跑道（上游 issue #37）

**症状**：App 上「计入成绩次数 - 次」一直是 0，但里程能凑出来、地图上像跑过了一圈。
**服务端判定**：点位顺序不对 ⇒ 这条**不计有效次数**（注意：不计次数 ≠ 判违规，
两回事，别混）。

**根因在 pace-forge 侧，不在插件侧**：`try_stadium_track` 拿打卡点的外接矩形
去拟合「跑道」，拟合出来的圈和真实点位对不上 ⇒ 生成的轨迹经过点位的顺序与服务端
期望的不一致 ⇒ 顺序校验失败。`track_mode = "stadium"` 走的就是这条路。

原先的判据只有**长宽比上限 2.5**（防「太瘦」），**没有下限**，于是「太圆」的形状漏了过去：

| 外框 | 长宽比 | 结论 |
| :--- | :--- | :--- |
| 标准 400m 跑道 177×92.5 | 1.91 | 真跑道，拟合正确 |
| 280m 校内小场 160×75 | 2.13 | 真小场，拟合正确 |
| 100×60 的小场地 | 1.67 | 真场地，拟合正确 |
| **实测某校 130.8×93.5** | **1.40** | **不是跑道**，却被「小场拟合」擦边命中 |

「小场拟合」原本是**两个独立容差**：`(|major-160| < 30) && (|minor-75| < 20)`
—— 实测值 `|130.8-160| = 29.2 < 30`、`|93.5-75| = 18.5 < 20`，
**两条各自差一点就过了** ⇒ 按半径 26m 生成（真跑道 36.8m）
⇒ 3 个打卡点落到圈外 16~20m ⇒ 只能硬吸附 ⇒ 轨迹凹向点位
⇒ **里程缩水 11.6%**（目标 2000m 只跑出 1768m），地图上一堆尖刺。

#### 修法（fork `windy664/pace-forge`，提交 `9efd7d8`）

上游 `IRmsk/pace-forge` 的 `src/api/flow.rs`，两处判据：

1. **长宽比下限 1.5**：环形跑道是「直道 + 两端半圆」，长轴必明显长于短轴
   （上面三类都在 1.5 以上）⇒ 拦住 1.40，且不误伤真跑道；
2. **`looks_small_oval` 加形状一致性**（`minor <= major * 0.55`）：
   真实小场 160×75 ⇒ 0.47；实测 130.8×93.5 ⇒ 0.715 ⇒ 不再擦边命中。

**只影响「本来就会拟错」的输入**：标准 400m 与 280m 小场两条分支在新判据之前
就已经 `return`，判定一字未变（新增 3 条测试钉住：太圆被拦 / 标准 400m 照旧拟合 /
小场照旧拟合）。上游 **524 条测试全绿**。

#### 效果（实测，同账号、同参数 2km / 12min）

| | 修复前 | 修复后 |
| :--- | :--- | :--- |
| 拟合分支 | 「像 280m 校内小场，按半径 26m 生成」 | 「外框 131×93m（长宽比 1.4）太圆，不像环形跑道，改用路线规划」 |
| 吸附位移（最远） | 20 m | **4.5 ~ 6.1 m** |
| 实测里程 vs 目标 | 1768 m（**-11.6%**） | **1929 ~ 1993 m（-3.5% ~ -0.4%）** |
| 点位首次经过顺序 | 正确 | 正确 |

修复与数据已贴到上游
[issue #37 评论](https://github.com/IRmsk/pace-forge/issues/37#issuecomment-7205242737)。

#### 残余缺陷（不是判据问题，是该校点位分布的锅）

路线规划分支偶尔会打出 `[roads] 拉 OSM 路网失败（Overpass 10 分钟节流）`，
接着退化成**直线串点**（点 1 到点 2 之间路网本身不连通）⇒ 轨迹仍有尖刺、
里程仍有缩水（实测 **-0.4% ~ -3.5%** 波动）。这是**该校区打卡点分布在楼群之间、
路网连不通**造成的，不是形状判据能解决的。

### fork 与构建流程（为什么不用官方 release）

上游 `IRmsk/pace-forge` 的 **release 还没包含 main 上的真机对齐修复**，所以自己 fork：
`windy664/pace-forge`（本机 clone 在 `/tmp/pace-forge`，提交 `9efd7d8`）。

⚠️ **fork 的 Actions push 不自动触发**（实测两次都如此）⇒ 必须手动 dispatch：

```bash
cd /tmp/pace-forge
git push origin main
gh workflow run release.yml -R windy664/pace-forge --ref main
gh run download <run_id> -R windy664/pace-forge --pattern "pace-forge-linux-x86_64-*"
```

⚠️ `/tmp` 是 16G 的 tmpfs，构建缓存容易撑满 ⇒ 链接器报 `Bus error`。
构建 target 放 `~/Projects/Ru41-target`。

生产机侧（`~/apps/pace-forge/`）：

| 项 | 值 |
| :--- | :--- |
| 镜像标签 | `pace-forge:0.1.11-ru41.2`（upstream main 真机对齐 + 本次形状判据修复） |
| 上版备份 | `pace-forge.bak-0.1.11-ru41.1`（形状判据修复前）、`pace-forge.bak-0.1.11-official` |
| 重建脚本 | `~/Projects/zhihuishu-sidecar/deploy/start-sidecar.sh` |

> `start-sidecar.sh` 有个必做的前置：**先清 `/tmp/.X99-lock` 和 `/tmp/.X11-unix/X99`
> 再起 Xvfb**，否则 Xvfb 起不来、容器不健康（已配 `restart: unless-stopped`）。

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

## 提交后自动事后复查（提交成功 ≠ 安全）

**为什么需要**：服务端的跑批判定是**事后**做的（上游 issue #51 实测：提交时六项规则
全绿的记录，4~7 天后仍被判 `isCheat=1`、`appealStatus=4`、申诉通道关闭）。
而 `/api/cheat` 只能告诉你「现在被标了几次」，**告诉不了你哪一条、什么时候被标的**
—— 每条记录详情里的 `reasonList` 才有「运动数据有问题」。

**做法**（`plugins/campusrun/src/review.rs`）：每轮轮询先把**复查窗口内**的记录
逐条拉详情看有没有被回扫判违规，判了就用 `NotifyChannel` 推一条 QQ。

### 复查哪些记录：以服务端记录列表为准（2026-11-27 改）

| | 旧做法 | 现在 |
| :--- | :--- | :--- |
| 复查对象 | 本地台账里的「上次提交」那一条 | `/api/records` 里 `startTime` 落在 **[现在-14 天, 现在-3 天]** 的记录 |
| 第二次提交 | **把第一次挤掉** ⇒ 漏复查 | 两条都查 |
| 从 App 提交的记录 | **根本不在本地台账里** ⇒ 漏复查 | 一样查得到 |

本地台账仍留着（`campusrun.submit/last-submit`），但它现在只管**静默窗口**，
不参与复查的选目标。

### 复查节奏

| 时刻 | 行为 |
| :--- | :--- |
| 提交后 0~3 天 | 不查（上游实测违规出现在第 4~7 天，太早查没意义） |
| 第 3 天起 | 开始查。**没判违规也记一笔，但继续查**（每 24 小时一次） |
| 判了违规 | 记一笔 + 推一条，**同一个 rrid 只推一次** |
| 过了 14 天 | 不再查（那时服务端的判定基本定型） |

⚠️ **「查了没判违规就一查了之」是个 bug，2026-11-27 修掉了**：
旧实现「查了就记账、之后直接跳过」⇒ 第 3 天查干净、第 5 天才被判违规的那条
**永远不会被通知**——而这正是上游 issue #51 说的回扫形态。
回归测试：`review_rechecks_a_clean_record_and_catches_a_late_flag`
（假控制台先返干净、隔天返违规）。

### ⚠️ 落账必须带 revision（同一类坑，第二次踩）

`put_document(kind, id, expected, value)` 的 `expected = None` 是 **insert-or-ignore**：
文档已存在时**根本不覆盖**。旧代码写复查台账就是传 `None`，因为「每个 rrid 只写一次」
所以一直没暴露；一旦支持复查更新，第二次写就被丢掉 ⇒ 台账里永远停在第一次的
`flagged=false`，后面判了违规也改不掉。

**现在带上已有文档的 revision**（CAS），与 `submit.rs` 的 `save_pending` 一个写法。
这个 bug 是**回归测试抓出来的**，不是看代码看出来的。

⚠️ **落账失败就不推**：`put_document` 返回 `false`（版本冲突）说明台账没写进去
⇒ 下一轮还会再查、再推 ⇒ 同一条会被推两遍。宁可这轮不推（下一轮补推）。

### 边界（硬规矩）：只转述服务端事实，不编因果

- 只说服务端说了什么：判违规依据、申诉状态、能不能申诉、没达标的门槛项；
- **不**解释「为什么服务端觉得有问题」——那是服务端的事，我们只能转述；
- 单测 `notice_transcribes_server_facts_only` 钉住：正文里不许出现「因为轨迹」这类解释。

### 复查放在 `poll()` 最前面

不能被后面的取数失败连累（提交 `0297b04`）。账号没登录时复查会失败并打日志，
但不影响其它环节——生产机日志里的
`[campusrun] 复查失败：pace-forge 返回错误：请先登录`
就是**账号被用户登出**导致的，不是代码问题。

## 当前阻塞：账号被判违规 3 次（2026-11-27）

服务端把当前账号标记为违规 3 次，**提交被硬拦**：

```
服务端已把当前账号标记为违规（违规 3 次）：先处理，别继续提交
```

| 项 | 值 |
| :--- | :--- |
| 三次违规的时间 | 11-16 14:15 / 11-17 07:07 / 11-21 06:57 |
| `reasonList` 里唯一失败项 | 「运动数据有问题」 |
| `canAppeal` | **0**（申诉通道已关闭） |
| `appealStatus` | 1 |

⇒ 只能去 App / 找学校处理。**代码侧已就绪**：解封后跑一次就能验证
「学期有效次数 +1」（这正是 issue #37 那条线的验收口径）。

⚠️ **别拿「直穿 `/api/v78/cheat/cheatlist` 返回空」当「无违规」的证据**：
手动直穿缺 `unid`/`sid` 时返回 `{"self":null,"list":[]}`，那是**参数没带对**，
不是「你干净」。要拿 `self.count` 就得带全参数（`ru41 campusrun cheat` 就带）。

## 踩过的坑

- **`ru41 <模块> run` 会被宿主抢走**：宿主把 `run` 当成 tracker 的长驻任务（`poll|run`
  归宿主分发），插件的 cli 钩子根本收不到 → 提交子命令叫 `submit`，不叫 `run`。
  （2026-11-17 上线时踩到：`ru41 campusrun run --laps 3` 跑成了 tracker 的常驻任务。）
- **预演每次的轨迹都不一样**：`/api/plan` 每次重新生成（随机种子），
  所以「预演通过」不等于「提交的那条也长这样」——好在 `/api/run` 自己会再跑一遍
  preflight，自检不过它自己就不提交。
- **`put_document` 的 `expected=None` 是 insert-or-ignore**（2026-11-27 第二次踩）：
  复查台账第二次写被丢掉 ⇒ 台账停在 `flagged=false`。
  写更新一律带 revision（CAS），与 `submit.rs` 的 `save_pending` 同写法。
- **pace-forge 的登录是单设备锁**：账号在手机 App 登录时，PC 侧
  `/api/accounts/{id}/login` 会返回 `10121`（服务端账号锁，要等 1 小时或让原设备退出登录）。

## 已知限制

- 依赖 pace-forge 控制台在跑；控制台挂了插件就查不到（错误会直说「连不上」）。
- 规则面（必过点/围栏/补跑窗口）走 `rules` 与 `plan` 拿；`plan` 会消耗打卡点接口配额
  （服务端限 5 分钟 3 次），别高频连着调。
- 记录详情（`/api/record/{rrid}`）的轨迹字段很大，插件不进 agent 上下文。
- 提交结果**未知**（网络断了、服务端可能已入账）时，插件标 `unknown` 且不可重试，
  需要去运动记录页核对——这一步只能人做。
- 插件看不到 pace-forge 自己的提交台账（它在宿主的数据目录里），所以插件自建一份
  `campusrun.submit` 记录当台账（现在只用于静默窗口）。
- **复查要 pace-forge 处于登录态**：账号没登录时复查会失败（生产机日志里
  `复查 … 失败：…请先登录`）⇒ **跑完要立刻登出把账号还给用户**，否则复查一直是坏的。

## 部署

插件是 cdylib，按 `docs/plugin-development.md` 的规矩编（容器内 `rust:1.98-trixie`），
产物 `plugins/target-plugins/release/libru41_plugin_campusrun.so`，
scp 到宿主 `~/ru41-mimo-smoke/plugins/` 后重启需要它的容器（QQ 命令在 `ru41-qq`）。

### campusrun 的常驻容器（2026-11-26 加）

之前 campusrun **没有**自己的容器 ⇒ 它的 tracker 从没跑过：
「每小时拉一次学期进度」与「提交后第 3 天自动复查」都落不了地。

```bash
bash deploy/start-campusrun.sh     # 起/重建 ru41-campusrun 容器
```

- 容器内跑 `ru41 schedule run campusrun`（**只读轮询 + 事后复查**）；
- `PACE_FORGE_TOKEN` **从现有容器 env 直接取**（真值只在主机内存里过一手，
  不落盘、不进 git、不进日志）；取不到会直接报错退出，别让它静默跑成一个坏容器；
- 容器 `--restart unless-stopped`，宿主重启后自己回来。

