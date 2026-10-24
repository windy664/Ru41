---
type: module
status: ready
module: jwxt
source: G 校教务系统（正方 jwglxt，经联奕 CAS 单点登录）
updated: 2026-10-20
tags: [Ru41, jwxt, 课表, QQ, source]
---

# 教务课表监控（jwxt）

对接 G 校教务系统，拉取个人课表并消费：
**课表变更检测（调课/停课提醒）**、**每日明日课表推送**、**QQ 命令查询**。
协议细节与登录链路见 [jwxt-recon.md](jwxt-recon.md)。

## 能力

| 能力 | 说明 |
|---|---|
| 课表变更检测 | 对比整学期快照（`jwxt_timetable`），换教室/调周次/停课即推送 diff |
| 明日课表推送 | 每日到 `remind_hour`（默认 21 点）后推一次，同日去重 |
| **日程同步** | `sync_schedule = true` 后把本周+下周课程展开成日程事件：课前 20 分钟自动提醒、`/日程` 可见；调课自动取消旧事件重建 |
| QQ 查询 | `/课表 [明天|周]`、自然语言「明天有什么课」；读缓存不触网，仅单聊 |
| 自动登录 | 验证码算术题本地求解（模板匹配），TGT/会话失效自动重建，无需人工 |
| 手动兜底 | `ru41 jwxt login` 下载验证码图 → `--captcha <答案>` 人工完成登录 |

红线：**只读**（只拉课表，不选课不评教）；失败不重试风暴（CAS 有失败计数与
账号锁定）；凭证只存 gitignored 本地配置或环境变量；会话文件等同登录态，
只落 `.ru41/`。

## 登录模型（全自动）

校方 CAS（联奕）账密登录强制**算术图形验证码**（如 `9 * 2 =`），且 TGT 实测
时效很短（分钟级），因此模块把登录做成**全链路自动**：

1. 轮询时优先复用持久化的教务会话 cookie → 其次用持久化 TGT 换 ST（免验证码）；
2. 都失效时：下载验证码 → **本地模板匹配求解算术题**（`captcha.rs`，
   字形库由 40 张人工标注样本训练，LOO 40/40）→ 提交登录 → 持久化新 TGT；
3. 识别不确定（字形距模板过远）或答案被拒时换新图重试，单轮最多 3 张；
   **密码错误/账号锁定立即停手不重试**，并推送告警（同日去重）。

手动兜底仍在：`ru41 jwxt login` 下载验证码图，人工读数后
`ru41 jwxt login --captcha <答案>` 提交（验证码风格变化时使用，同时应更新
tests/fixtures/jwxt_captcha/ 样本与模板库）。

## 配置（ru41.toml）

```toml
[sources.jwxt]
# 推荐环境变量注入：RU41_JWXT_USERNAME / RU41_JWXT_PASSWORD（统一认证账号）
# username = "2640xxxxxx"
# password = "…"
semester_start = "2026-09-07"   # 学期第一周周一；周次推算与明日课表依赖它
# xnm = "2026"                  # 学年起始年；默认按日期自动推断
# xqm = "3"                     # 学期码 3=一/12=二/16=三；默认自动推断
# interval_secs = 3600          # jwxt run 轮询间隔
# remind_hour = 21              # 明日课表推送的本地时间下限
# daily_remind = true           # 到点推明日课表
# cas_base = "https://cas.gzus.edu.cn/lyuapServer"
# jwxt_base = "https://jwxt.gzus.edu.cn"
# session_path = ".ru41/jwxt/session.json"

# 日程同步（课前 20 分钟提醒、/日程 可见）：
# sync_schedule = true
# class_duration_mins = 40      # 单节时长（80 分钟一大节 = 2 小节）
# section_times = [             # 第 1..=16 节的上课时刻（教务接口不暴露，按学校作息填）
#   "09:00", "09:40", "10:40", "11:20",
#   "12:30", "13:10", "14:00", "14:40",
#   "15:30", "16:10", "17:00", "17:40",
#   "19:00", "19:40", "20:30", "21:10",
# ]
```

- `semester_start` 不配时：变更检测照常工作，但明日课表推送与 QQ 按周次
  过滤不可用（模块会提示）。
- `sync_schedule = true` 但缺 `semester_start`/`section_times`：同步不生效，
  模块推送一次配置提示（不每日骚扰）。
- 日程同步只展开**本周+下周**（周日晚即可看到下周一的课），事件粒度为
  单次课；调课/停课按 `课程号|周|星期|节次|教室|教师` 签名 diff，旧事件取消、
  新事件重建；`reminded_starts` 保留，不会重复提醒。
- `xnm`/`xqm` 自动推断规则：9-12 月 → 当年第一学期；1 月 → 去年第一学期；
  2-8 月 → 去年第二学期。短学期（16）不自动推断，需要时显式配置。
- CAS 前端 RSA 公钥（`cas_modulus`/`cas_exponent`）有内置默认值（该校部署
  配置），校方改版时才需要覆盖。

## CLI

```bash
ru41 jwxt login [--captcha <答案>]   # 人工兜底（正常不需要；poll/run 自动登录）
ru41 jwxt poll                       # 拉一轮：变更检测 + 到点推明日课表（cron 友好）
ru41 jwxt run                        # 常驻，按 interval_secs 轮询（schedule 可见）
```

## 消息格式

提醒与 QQ 回复都按 markdown 输出（QQ 通道先试 markdown，平台不支持时自动降级纯文本），
课程行显示**具体钟点**而不是节次：

```
## 📚 明日课表

**10月21日 周三 · 第7周**

- **09:00-10:20** 高等数学I(理) @U101（张晓婷）
- **17:00-18:20** 国家安全教育 @D102（黄秋冬）

共 2 节课
```

时间由 `section_times` + `class_duration_mins` 换算；未配置作息表时回退成 `3-4节`
文本（`/日程` 与同步事件时间不受影响——没有时刻表也就不做日程同步）。

## 数据与幂等

- `jwxt_timetable`（key `{xnm}#{xqm}`）：整学期快照 + 签名 hash；读写收拢在
  模块内，QQ 命令读同一份。
- `jwxt_alert`：`daily:{date}` 明日课表同日去重；`login:{date}` 登录失效提醒
  同日去重。
- 变更粒度：`课程号|星期|节次|周次|教室|教师` 签名；任一字段变化都算变更
  （报告为旧签名取消 + 新签名新增两条）。
- 单轮 poll 幂等：重复执行不会产生重复提醒。

## 测试

`tests/jwxt_sources.rs`：本地 axum mock 全链路（CAS REST 登录表单校验、
TGT 复用、课表解析、未登录 302 判定、变更 diff、每日去重、失效提醒去重、
TOML 未知字段拒绝）；`src/modules/jwxt/source.rs` 单测含与校方前端 JS
逐字节对照的 RSA 加密向量。不访问真实教务。
