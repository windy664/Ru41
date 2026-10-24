# 微助教协议调研（2026-10）

目标：为 Ru41 新增微助教（teachermate）附属选择实现路径，对标学习通/智慧树附属。
结论先行：**纯 Rust 直连**（`modules/teachermate/`，形态同 chaoxing）。微助教协议是活的纯 HTTP
+ 一个 faye WebSocket，无浏览器、无验证码、无签名，智慧树式 sidecar 的前提（协议死）不成立。
与学习通的关键差异：**凭证是人工投喂的 openid，无账密重登路径**，绑定与失效告警是设计重心。

> 注意：本次为纯网上调研（开源实现源码交叉验证，2018→2026 项目持续可用证明协议稳定），
> 未做真机抓包实测；动手前应先用探针验证端点与响应 schema（见文末待实测清单）。

## 平台形态

- 学生端无独立 APP，签到/答题/讨论全在微信服务号内嵌 WebView 完成；
  学生 API 基址 `v18.teachermate.cn`，PC 网页端与长连接基址 `www.teachermate.com.cn`。
- PC 网页版 `/pc-ssr/student/homework` 存在但作业靠"作业验证码"进入，无登录态——
  微信服务号 openid 是学生侧唯一凭证入口。
- 服务号菜单链接形如 `https://v18.teachermate.cn/wechat-pro-ssr/student/sign?openid=<32位>`，
  openid 即从 URL 提取。

## 生态摸底

| 项目 | 状态 | 结论 |
| --- | --- | --- |
| ManiaciaChao/yatm (145★) | 2025-03 仍在更新 | TS 实现协议最全：active_signs/签到/role + faye 二维码 + 微信 devtools 自动扫码；`docs/AcquireOpenID.md` 讲 4 种取 openid 手法，实现主要参考 |
| Azuka753/wzj_sign_public → YunHai121/wzj_signin | fork 链，2026-09 更新 | Go+Redis+Web+邮件；证明 2024-2026 协议未变；普通 Edge UA 直连成功（无需微信 UA）；二维码页自动刷新方案可借鉴 |
| twchn/teachermate-auto-check-in (28★) | 2018 | `docs/teachermate_API_reference.md` 是唯一成文 API 参考（courses/students/role） |
| Adsicmes/teachermateAutoAnswer | 2024 | mitmproxy 抓包确认 v3 papers 题目接口；仅参考，不做代答 |
| YukiNasumi/HUST-TeacherMate-Asistant | 2026-04 | yatm fork；内置教学楼坐标表 gps.json 的思路可借鉴（预设常用签到坐标） |
| zn-cn/wzj-sign-in-weixin、BeBeBerr/teachermate-check-in、recolic/micro-teaching-assistant-fucker | 老 | 无增量协议信息 |

教训（承接 chaoxing 摸底）：搜索快照标"活跃"不可信，以 GitHub API `pushed_at` 为准。

## 认证模型：openid（与学习通的根本差异）

- 传递方式为 **HTTP Header `Openid`**（GET/POST 同），非 cookie、非 body 参数；
  建议附带 Referer 指向对应 SSR 页（yatm 做法），UA 用普通桌面 UA 即可。
- 失效响应特征串：`{"message":"登录信息失效，请退出后重试"}`。
- openid 生命周期：
  1. 微信里再点任何微助教链接 → 重新 OAuth，旧 openid 立即失效；
  2. 二维码签到被扫（本人微信扫推送的 qrUrl）→ openid 消耗失效；
  3. 社区报告约 2 小时的"玄学失效"。
- ⇒ 无账密重登，只能人工从服务号重新提取投喂。模块需要"绑定 openid"命令
  （可复用 QQ 渠道 bind_secret 口令机制）、openid 持久化 `.ru41/teachermate/`（gitignored）、
  失效每日最多一次告警（复用 `chaoxing_alert` 的 `login:{date}` 去重模式）。

## 协议端点（来源：yatm/wzj_sign_public 源码 + twchn API 参考，未实测）

均 `https://v18.teachermate.cn`，Header 带 `Openid`：

1. **待签列表** `GET /wechat-api/v1/class-attendance/student/active_signs`
   - 响应数组：`{courseId, signId, isGPS(0/1), isQR(0/1), name(课程名), code, startYear, term, cover}`
   - 签到类型只有 普通/GPS/二维码 三种（无学习通式拍照/手势）；
   - yatm 硬编码 `If-None-Match` 防 304 缓存吞掉新签到，实现时注意。
2. **签到** `POST /wechat-api/v1/class-attendance/student-sign-in`
   - JSON body：`{courseId, signId, lon?, lat?}`（GPS 才带坐标）
   - 成功响应含 `studentRank`（签到名次）；重复签到返回文本"你已经签到成功"；
     yatm 把 `errorCode===305` 也按已签处理；
   - 实测经验（wzj_sign_public）：GPS 签到发普通载荷也能成功，服务端坐标校验弱。
3. **存活探测** `GET /wechat-api/v2/students/role`：学籍信息数组；失效时返回 `{message}`，
   请求轻，适合做轮询前的 openid 存活检查。
4. **课程列表** `GET /wechat-api/v1/students/courses`：课堂 id/名称/教师/课堂编码。
5. **个人信息** `GET /wechat-api/v2/students`：超大 JSON（候选项一次性全量返回），
   仅绑定回执时用，不适合高频轮询。
6. **题目** `GET /wechat-api/v3/students/papers/{paperId}/questions?page=N`（2024 mitm 抓包确认）；
   答题红线见下，不自动作答，仅作业/测验提醒可用。
7. **二维码签到长连接** `wss://www.teachermate.com.cn/faye`（同一 URL 支持 long-polling POST）
   - faye/cometd 协议：`/meta/handshake` → `/meta/connect` → `/meta/subscribe`
     channel `/attendance/{courseId}/{signId}/qr`，**无需任何鉴权**（courseId/signId 来自 active_signs）；
   - 服务端推送：type=1 带轮换的 `qrUrl`（约 10s 级轮换，等价教师端投屏二维码），
     type=3 为学生签到结果（含 rank）；
   - 正确姿势：本地渲染 qrUrl 为二维码推送给本人，用微信扫一扫完成——**无法凭空代签**。

## 关键决策记录

- **放弃 sidecar**：同学习通逻辑——协议活、纯 HTTP，Rust 直连少一个进程/文件桥/venv；
  faye 二维码在 Rust 内用 WebSocket + QR 渲染即可（本地终端/推送图片，或复用智慧树
  sidecar 的报告文件形态对外提供 qr.html 页面亦可，首版从简）。
- **凭证模型单独设计**：学习通靠账密+cookie 复用，微助教只有一次性 openid；
  绑定命令投喂、失效检测（特征串）、失效告警每日去重、openid 落盘 gitignored，
  四件事是模块骨架的第一优先级。
- **轮询预算**：active_signs 请求极轻；签到窗口 1-2 分钟，默认 60s（与 chaoxing 对齐）；
  存活探测可与 active_signs 同轮进行（按失效特征串判断即可，无需独立请求）。
- **风控红线**（对齐 chaoxing）：默认只提醒，自动签开关默认关；二维码交本人扫；
  不自动作答（自动答题=代写）；轮询间隔不小于 60s，避免高频请求被发现。
- **坐标预设**：借 HUST fork 思路，`default_lat/lon/addr` 缺省坐标 + 可选地点表，
  与 chaoxing 配置字段命名保持一致，降低配置心智。

## 实测记录（2026-10-24，本机直连探针）

1. ✅ openid 失效 = **HTTP 401 + 特征串**：缺失/伪造 openid 请求 active_signs 均返回
   `401 {"message":"登录信息失效，请退出后重试"}`。client 按「401 或特征串」双保险判定。
2. ✅ faye 长连接存活：`/meta/handshake` 正常发 clientId；用伪造 course/signId 订阅
   `/attendance/999999/888888/qr` 返回 `successful=true`——通道无需鉴权、订阅不校验
   真实性（二维码消息只在真实签到进行时推送）；`/meta/connect` advice
   `timeout=15000ms`，心跳间隔取其一半 ≈7.5s（同 yatm）。二维码推送路线可行。
3. ✅ **二维码签到机制（2026-10-24 从 wechat-pro-ssr chunk 39 反编译）**：
   `wx.scanQRCode(result => window.location.href = result)`——码的内容是一个
   轮换 URL，"扫码"的本质是**用微信身份打开它**，页面跳转即完成签到，
   没有"提交 token"型接口。推论：若 qrUrl 认 openid（全线 wechat-api 均认），
   则 faye 实时收码 + 带 openid 请求该 URL = 全自动替扫；若走微信 OAuth
   会话绑定则不可行。**待一个真实 qrUrl 样本定案**（faye 抓码或 mitmdump
   抓扫码请求均可；wzj_signin/yatm 均止步于"转码给人扫"，无先例可抄）。

## 作业接口（2026-10-24 从前端 JS bundle 逆向）

PC 网页端（`/pc-ssr`）与微信端（`/wechat-pro-ssr`）chunk 均为公开静态资源，从中提取：

- `GET /wechat-api/v1/homework/courses` —— 有作业的课程列表（监控入口）
- `GET /wechat-api/v1/homework?courseId={cId}` —— 单课作业清单，
  响应 `{homeworkList: [...], assessments: [...]}`
- `GET /wechat-api/v1/homework/{hId}` —— 作业详情
- `GET /wechat-api/v1/homework/{hId}/code?post_submit=` —— 作业验证码（PC 端凭码进入）
- `GET /wechat-api/v3/students/papers/{paperId}/questions?page=N` —— 题目（AutoAnswer 抓包）
- PC 端作业按「作业验证码」拉取（`/v1/homework/no-auth/code/{code}`），验证码只有老师
  发放时才可知——**自动监控必须走微信端 openid 通道**，验证码路径不可用。

作业条目字段（UI 代码确认）：`{id, name, status, deadline, submitTime, homeworkId, passed,
allowOvertime, isPostSubmit}`。状态机（微信端 UI 状态表）：

| status | 含义 | 处理 |
| --- | --- | --- |
| 1 | 未完成 | 需动作：新发现 📝 + 临期/过期提醒 |
| 2 | 被打回 | 需动作：❗ 重交警告 |
| 3 | 已超时 | 漏交警示一次 |
| 4/5 | 已提交（可修改/评分中） | ✅ 回执 + 待办回勾 |
| 6/8 | 已公布成绩/已批阅 | ✅ 回执 + 待办回勾 |
| 7 | 补交中（老师允许） | 📝 补交窗口提示 |

待实测清单收敛：~~响应信封~~ ✅ 已实测——homework/courses 返回
`{"courses":[]}`（2026-10-24 真实 openid），解析层三形态全兼容；
~~deadline 格式~~ 仍待首个真实作业样本；另实测 windy2→v18 网络偶发
HTTP 000（连接被重置），客户端已加 GET 三次重试（0/300ms/1.2s）。

## 待实测清单（需真实 openid，bind 命令即探针）

1. `errorCode` 305 的准确语义（已签？类型不符？）。
2. 作业接口响应信封与 deadline 格式的实测确认（解析层已宽容，真实样本到手后收紧）。
3. `Openid` header 大小写敏感性、必需 Referer/UA 组合的最小集。
4. 2026 年微信新版服务号取 openid 的最新操作路径（yatm 的 4 种手法仍适用与否）。
