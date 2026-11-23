# campusrun 插件层：目录树与职责边界

> 2026-11-23 整理。起因是插件层**自己算了一个「学期目标」**并据此说「已达标」——
> 那是插件在后端之上加的判断，后端并没有给过这个数（上游 `semesterGoal` 的单位是米，
> 我们这个课的值是 1 米 ≈ 后台没设目标）。这份文档把职责边界写清楚：
> **插件只转述后端，后端没说的，插件也不许编。**

## 目录树

```
plugins/campusrun/
├── Cargo.toml                    # 插件 crate（cdylib，宿主 linkme 加载）
├── config.example.toml           # 配置模板：base_url / token / 允许提交 / 配速区间 / 海拔 / 静默窗口
├── skills/                       # agent 怎么用这些命令（**策略在这一层，不在插件里**）
│   ├── campus-run-progress/SKILL.md    # 进度与规则类问答 + 「跑一次」的流程与参数
│   └── campus-run-records/SKILL.md     # 历史记录与违规状态类问答
├── src/
│   ├── lib.rs                    # 模块注册：调度/命令/QQ 钩子怎么挂进宿主
│   ├── cli.rs                    # `ru41 campusrun <子命令>` 的分发
│   ├── qq_command.rs             # QQ 私聊里的同一套命令
│   ├── client.rs                 # 与 pace-forge 控制台说话（HTTP + token + 解包）
│   ├── model.rs                  # 上游数据结构（只做反序列化，不加解释）
│   ├── render.rs                 # 把上游数据渲染成 Markdown
│   ├── service.rs                # 取数 → 渲染；提交链：换算 → 门槛 → 预演 → 确认 → 提交
│   ├── submit.rs                 # 距离/圈数 → 提交参数（时长、步数、落跑时刻）
│   └── config.rs                 # 配置项定义
└── tests/campusrun.rs            # 集成测试（起一个假控制台，验命令输出）
```

## 每一层做什么、不做什么

| 文件 | 做 | **不做**（越界就是 bug） |
|---|---|---|
| `client.rs` | 打控制台的 `/api/*`，解包、鉴权、错误映射 | 不解释业务含义 |
| `model.rs` | 上游 JSON → 结构体 | **不加工字段语义**（不换算单位、不推导结论） |
| `render.rs` | 结构体 → Markdown | **不下结论**：不判「已达标」、不猜「为什么次数是 0」 |
| `service.rs` | 编排：取数 → 渲染；提交链的先后顺序 | 不自己算门槛（门槛一律从服务端读） |
| `submit.rs` | 把「跑 3 公里 / 跑 8 圈」换算成提交参数 | 不改用户要的量 |
| `config.rs` / `config.example.toml` | 连接信息、开关、**提交参数**（配速区间、海拔、静默窗口） | **不存课程要求**（学期公里目标、次数目标不是插件该管的事） |
| `skills/*` | agent 的判据与话术：读哪些字段、怎么转述、什么不许说 | 不写进插件代码 |
| `cli.rs` / `qq_command.rs` | 命令分发 | 不做业务判断 |

## 三条硬规矩

1. **后端给什么就转述什么。** 上游说 `semesterGoal=1`（米），插件就说「1 米 ≈
   这个课没设记分跑学期目标」，**不许**把它当公里算成「已达标」。

2. **后端没解释的，插件也不许替它解释。** 上游只给了「有效次数 0」，
   插件只报这个数；**不写**「因为单次距离没够门槛」这种自己编的因果
   （实测有 4 条记录 `complete=true` 而次数仍是 0，那个因果不成立）。

3. **课程要求不是插件的事。** 学期跑量要求（几十公里）在 App/教学计划上，
   协议层读不到（`/api/v41/running/getPersonalSemesterInfo` 的真实响应只有 7 个字段，
   `semesterTargetDis`/`semesterTargetCount` 都是空的）。**不许**在插件配置里造一个
   目标值；用户给了 App 上的数字，就由 **agent 在对话里**用它算「还差多少」，
   并说明是 App 口径。

## 后端（pace-forge）才是协议层

插件一行协议都不自己实现，全部走 pace-forge 控制台：

| 我们要的东西 | 后端的接口 |
|---|---|
| 学期进度/目标 | `POST /api/v41/running/getPersonalSemesterInfo` |
| 学期累计里程/次数 | `POST /api/v55/runnings/recordssummary/semester` |
| 记录列表 / 单条详情（含逐条规则判定） | `POST /api/v70230/runnings/records`、`/api/v70320/runnings/get_one_record` |
| 违规自查 | `POST /api/v78/cheat/cheatlist` |
| 本次规则（最小里程/配速区间/步频区间/必经点） | `POST /api/v70103/runModePolicy` |
| 打卡点 | `POST /api/v560/get/1/distance/{米}` |
| 轨迹生成 + 八类自检 + 提交 | pace-forge 自己的 `plan` / `dry` / `run` / `submit` |

新版本（v0.1.10）还给了 `/api/raw`：**带签名直穿上游任意 `/api/` 路径并回原文**——
排查「这个字段到底什么意思」时用它，不必再猜协议（2026-11-23 就是靠它确认了
`semesterGoal` 的单位是米、且响应里没有第二个目标字段）。
