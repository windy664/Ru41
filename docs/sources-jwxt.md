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
| QQ 查询 | `/课表 [明天|周]`、自然语言「明天有什么课」；读缓存不触网，仅单聊 |
| 手动登录 | `ru41 jwxt login` 下载验证码图 → `--captcha <答案>` 完成登录 |

红线：**只读**（只拉课表，不选课不评教）；失败不重试风暴（CAS 有失败计数与
账号锁定）；凭证只存 gitignored 本地配置或环境变量；会话文件等同登录态，
只落 `.ru41/`。

## 登录模型（为什么需要一次人工验证码）

校方 CAS（联奕）**每次账密登录都强制图形验证码**，无法无人值守重登。
因此模块把登录拆成「一次性人工 + 长期复用」：

```
ru41 jwxt login            # 下载验证码图片，打印路径
ru41 jwxt login --captcha 56   # 提交答案；成功后 TGT 持久化到 .ru41/jwxt/session.json
```

之后 `jwxt poll/run` 用持久化 TGT 静默换 ST 建会话，不再需要验证码。
TGT 过期（服务端时效，待运行观察）时模块推送「登录态失效」提醒（同日最多
一次），重新跑一遍 `jwxt login` 即可。

## 配置（ru41.toml）

```toml
[sources.jwxt]
# 推荐环境变量注入：RU41_JWXT_USERNAME / RU41_JWXT_PASSWORD（统一认证账号）
# username = "2640xxxxxx"
# password = "…"
semester_start = "2026-10-05"   # 学期第一周周一；周次推算与明日课表依赖它
# xnm = "2026"                  # 学年起始年；默认按日期自动推断
# xqm = "3"                     # 学期码 3=一/12=二/16=三；默认自动推断
# interval_secs = 3600          # jwxt run 轮询间隔
# remind_hour = 21              # 明日课表推送的本地时间下限
# daily_remind = true           # 到点推明日课表
# cas_base = "https://cas.gzus.edu.cn/lyuapServer"
# jwxt_base = "https://jwxt.gzus.edu.cn"
# session_path = ".ru41/jwxt/session.json"
```

- `semester_start` 不配时：变更检测照常工作，但明日课表推送与 QQ 按周次
  过滤不可用（模块会提示）。
- `xnm`/`xqm` 自动推断规则：9-12 月 → 当年第一学期；1 月 → 去年第一学期；
  2-8 月 → 去年第二学期。短学期（16）不自动推断，需要时显式配置。
- CAS 前端 RSA 公钥（`cas_modulus`/`cas_exponent`）有内置默认值（该校部署
  配置），校方改版时才需要覆盖。

## CLI

```bash
ru41 jwxt login [--captcha <答案>]   # 两步验证码登录
ru41 jwxt poll                       # 拉一轮：变更检测 + 到点推明日课表（cron 友好）
ru41 jwxt run                        # 常驻，按 interval_secs 轮询（schedule 可见）
```

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
