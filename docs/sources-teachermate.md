# 微助教签到+作业监控说明

微助教（teachermate）课程签到检测与作业监控。协议是活的纯 HTTP + 一个可选的
faye WebSocket（二维码，尚未接入），**不经过 sidecar**，协议端直连实现在 Rust 模块内，
形态与 ecard / chaoxing 一致。协议调研与证据链见 docs/teachermate-recon.md。

## 与学习通附属的关键差异

- **凭证是人工投喂的 openid**：学生侧无账密登录，openid 从微信服务号菜单链接的
  URL 参数提取，且在微信里再点任何微助教链接/扫码签到后立即失效。
  没有自动重登路径，失效时依赖告警指路 `teachermate bind` 重新投喂。
- **平台不暴露已签状态**：active_signs 只有进行中的签到（普通/GPS/二维码三种），
  是否已签只能由 sign_in 响应得知；因此「结束未签」回执是收敛推断（活动从
  进行中列表消失且本侧无签到记录），已在微信手签的用户按文案忽略即可。
- **二维码无法代签**：二维码由服务端轮换分发（等价教师端投屏），只提醒现场扫。

## 架构分工

- `modules/teachermate/source/client.rs`：协议客户端——openid Header 鉴权、
  `active_signs` 待签列表、`student-sign-in` 签到（普通/GPS）、`students/role` 学籍探测、
  `homework/courses` + `homework?courseId=` 作业清单；openid 失效以
  「HTTP 401 或平台特征串登录信息失效」识别（2026-10-24 实测）。
- `modules/teachermate/service/mod.rs`：签到轮询编排——新活动落 records 去重，
  auto_sign 开启时普通/GPS 当场自动签，消失的活动给结束回执，失效每日告警一次。
- `modules/teachermate/service/work.rs`：作业监控——按课程聚合作业清单，
  状态分类（1 未完成/2 被打回/7 补交中需动作），临期/过期每日提醒，
  待办同步与 Obsidian 课程目录同步（与学习通作业模块同构，快照字段对齐，
  跨平台作业总表直接聚合）。
- `modules/teachermate/cli.rs`：`bind` 命令（提取 openid → 落盘 → 学籍验证）。
- 凭证解析顺序：环境变量 `RU41_WZJ_OPENID` > `[sources.teachermate].openid` >
  openid 文件（bind 写入，gitignored 路径 `.ru41/teachermate/`）。

## 配置（ru41.toml）

```toml
[sources.teachermate]
# openid = ""                # 或环境变量 RU41_WZJ_OPENID；建议用 bind 命令落盘
# openid_path = ".ru41/teachermate/openid"
# interval_secs = 60         # 签到窗口常为 1-2 分钟，太疏会漏
# notify = true              # 待签活动/作业经 [notify] 默认出口推送
# auto_sign = false          # 发现待签当场自动签（普通/GPS；二维码除外）
# default_lat = 30.0         # GPS 签到缺省纬度
# default_lon = 114.0        # GPS 签到缺省经度
# work_poll_secs = 1800      # 作业轮询间隔（独立低频）
# work_due_days = 3          # 截止前 N 天开始临期提醒
# work_todo_sync = false     # 未交作业自动进待办、平台已交自动回勾
# work_vault_sync = false    # 作业清单同步进 Obsidian 课程目录
# vault_course_dir = "学期/2026秋"
# vault_courses = { "大学英语" = "大学英语" }
```

## 运行

```bash
ru41 teachermate bind "https://v18.teachermate.cn/wechat-pro-ssr/student/sign?openid=<32位>"
                            # 提取 openid、落盘并验证学籍；投喂后勿再点微信里的微助教链接
ru41 teachermate poll       # 采集一轮待签活动并按需提醒/自动签（cron 友好）
ru41 teachermate run        # 常驻轮询签到（schedule run teachermate 等价）
ru41 teachermate work       # 采集一轮作业清单并按状态提醒（schedule run teachermate-work）
ru41 teachermate sign 2002  # 手动签普通/GPS 活动（GPS 坐标缺省用 default_lat/lon）
ru41 teachermate sign 2002 --lat 30.1 --lon 120.2   # 显式覆盖位置
```

## 数据模型与幂等键

- records kind `teachermate_sign`，key `{courseId}#{signId}`：活动快照
  （课程、类型、是否 GPS/二维码、signed_at/signed_via、auto_note、ended_at）。
  首次发现入库；自动签成功/手动签写 signed_at，平台拒绝写 auto_note（不再重试），
  从进行中列表消失写 ended_at——三类标记各自只让对应回执发一次，天然去重。
- records kind `teachermate_work`，key `{courseId}#{hId}`：作业快照
  （course/title/status 数字/end_at/todo_id/submitted_at）。字段名与学习通作业快照对齐，
  跨平台作业总表（chaoxing work 模块渲染）直接按 status 数字判 open。
- records kind `teachermate_work_alert`，key `due|overdue:{date}:{key}`：临期/过期每日一次。
- records kind `teachermate_alert`，key `openid:{date}`：失效告警每日最多一次
  （签到与作业两个 tracker 共用，一天合计最多一条）。

## 通知策略

- **签到**：待签活动首次发现（⏰，二维码附「现场扫码」提示）、自动/手动签回执（✅❌）、
  活动结束收敛（🏁）。
- **作业**：新未完成作业（📝 带 deadline）、临期 ⏰ / 过期 ⚠️ 每日一次、
  提交 ✅（自动回勾待办）、被打回 ❗、补交窗口 📝。评分中→已公布等收敛性变迁只落库不播报。
- **待办**（work_todo_sync）：未完成作业进私人待办（`微助教｜课程｜作业名`，
  临期 High 优先级），平台已交自动回勾。

## 红线与边界

- 自动签只对普通/GPS 生效且默认关闭（auto_sign = false）；二维码永远只提醒现场扫。
- 不做自动答题、不做自动提交作业（代写红线，与学习通/智慧树附属一致）；
  作业只提醒/跟踪/归档。
- 互评任务（assessments）v1 只计数不提醒；作业正文与提交动作在微信服务号完成。
- 平台无手势/拍照/签到码签到类型；坐标校验弱是实测社区经验（recon 有记录），
  本模块仍按配置发送坐标，不做额外伪装。
- 作业响应信封与 deadline 格式未实测（解析层已宽容，见 recon 待实测清单）；
  二维码 faye 推送为后续扩展，尚未实现。

## 隐私边界

openid 是完整账号凭证：只存 gitignored 的 `.ru41/teachermate/openid` 或环境变量，
不进配置仓库、不进 Vault、不进日志；poll 报告只含活动 id 与课程名。
