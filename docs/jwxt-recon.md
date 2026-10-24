---
type: source-adapter
status: reverse-engineered
source: G 校教务系统（正方 jwglxt）经联奕 CAS 单点登录
updated: 2026-10-20
tags: [Ru41, jwxt, 课表, CAS, source]
---

# 教务系统（正方 jwglxt）协议说明

对 G 校教务 `https://jwxt.gzus.edu.cn/jwglxt/`（正方新教务，xxdm=12618）与
统一身份认证 `https://cas.gzus.edu.cn/lyuapServer/`（联奕 lyuap，React SPA）的
调查结果。**本文档只记录协议；学号、密码等个人参数存本地配置，不入库。**

## 结论：不走 ehall，直连 CAS → 教务

- `ehall.gzus.edu.cn` 是联奕事务中心，本身不承载课表数据；事务中心应用的跳转
  也是先过 CAS。
- 教务直登页 `login_slogin.html` 虽然可用（账号密码 + RSA PKCS#1 v1.5），但
  **教务密码与统一认证密码不同**（实测 CAS 密码返回「用户名或密码不正确」）。
- 浏览器会话记录还原出的真实链路：CAS 登录后以
  `service=https://jwxt.gzus.edu.cn/sso/lyiotlogin` 换 ST，教务验票 302 链：
  `sso/lyiotlogin` → `/jwglxt/ticketlogin?uid=..&timestamp=..&verify=..` →
  `login_slogin.html` → `index_initMenu.html?jsdm=xs&_t=..&echarts=1`，
  会话即建立。**用统一认证账号即可，不需要教务独立密码。**

## CAS（联奕 lyuapServer）REST 协议

SPA 前端（app.*.js）实际走 CAS REST，全程 form-urlencoded：

| 步骤 | 请求 | 响应 |
|---|---|---|
| 1. 登录 | `POST /v1/tickets` body `username`、`password=<ohdave RSA hex>`、`service=<服务URL>`、`loginType=`（可选 `id`、`code` 为验证码参数） | `{"tgt":"TGT-…","ticket":"ST-…"}`；失败时 `{"data":{"code":"CODEFALSE"}}` 等错误码 |
| 2. 换 ST | `POST /v1/tickets/<TGT>` body `service=<服务URL>&loginToken=loginToken` | 200，**body 就是 ST 字符串**（`ST-…`） |
| 3. 验票 | `GET <service>?ticket=<ST>` | 教务 302 链建立会话（JSESSIONID、route） |

- 错误码：`FALSE`（密码错）、`CODEFALSE`（验证码错/缺失）、`NOUSER`、
  `USERLOCK`、`USERDISABLED`、`TWOVERIFY` 等。
- **验证码**：该校 CAS `isVerifyCode=1`，账号密码登录必须带图形验证码；
  `GET /kaptcha?uid=<客户端生成的uuid>` 返回
  `{kaptchaType, uid, content: "data:image/png;base64,…", timeout}`。
  实测为**算术题图**（如 `8 * 7 =`，kaptchaType=1，100×25 PNG，彩色字形白底），
  答案填 `code`，`id` 填 kaptcha 响应的 `uid`。
- **loginType 探针**（2026-10-20）：`loginType=1` 会跳过验证码但**不接受**
  账密（返回 `PASSERROR`，应是短信码等其他登录类型）；账号密码只能走
  `loginType=""` + 验证码。无免验证码通道。
- **验证码自动求解**：`src/modules/jwxt/captcha.rs` 离线模板匹配（亮度阈值 245
  二值化 → 列投影切 4 个字形 → 归一化 12×16 位图 → 字形库汉明最近邻，
  距离 >48/192 判不确定拒答）。模板库由 40 张人工标注样本生成，
  LOO 交叉验证 40/40；fixtures 存 tests/fixtures/jwxt_captcha/。
- **TGT 短时效**：实测 TGT 约 20-45 分钟即被服务端判失效（“TGT 已失效”），
  教务会话闲置约半小时过期 → 自动验证码登录是常态路径而非兜底。
  TGT 换 ST **不需要验证码**。
- SSO 验票 302 链偶发抖动（同一 TGT 分钟内失败又恢复）：客户端做一次
  400ms 间隔的短重试，错误信息带最终状态码便于诊断。

### 密码加密（Ohdave BigInt RSA）

CAS SPA 内嵌 Ohdave RSA（BigInt.js，`setMaxDigits(131)`），配置里取 hex 参数
`modulus`（1024bit，`00b5ee…` 开头）与 `exponent`（`010001`）。`encryptedString`
语义（已用页面 JS 与 Python 移植逐字节对照验证）：

1. 把密码按 chunkSize = 2×h(modulus) 个字符分块（h 为最高非零 16-bit digit
   下标；不足补 `\0`）。
2. 每块构造整数 m = Σ char[i] × 256^i（即把 charCode 当字节做**小端**打包；
   与 UTF-16 无关，非 ASCII 密码行为未定义，密码应使用 ASCII）。
3. 密文块 c = m^e mod n（裸 RSA，无填充）。
4. 输出：每块转 hex，按 16-bit limb 自高位到低位每组 4 位（宽度 =
   (最高非零 limb 下标+1)×4，不补齐到固定宽度），多块用单个空格连接。

## 教务接口（正方 jwglxt）

会话 cookie：`JSESSIONID` + `route`（均挂在 jwxt 域）。未登录访问业务接口
302 → `login_slogin.html`，以此判定登录态失效。

| 接口 | 参数 | 说明 |
|---|---|---|
| `POST /jwglxt/kbcx/xskbcx_cxXsgrkb.html?gnmkdm=N2151` | form `xnm=<学年>`、`xqm=<学期码>`、`gnmkdm=N2151` | 学生个人课表，**一次返回整学期** |
| `GET /jwglxt/kbcx/xskbcx_cxXskbcxIndex.html?gnmkdm=N2151` | — | 课表页面（前端仅渲染，无按周参数） |

- 学期码 `xqm`：`3`=第一学期、`12`=第二学期、`16`=第三学期；`xnm` 为学年起始
  年（2026-2027 学年 → `2026`）。
- 响应顶层：`kbList`（行课条目）、`xsxx`（XNM/XQM/XH/XM/BJMC 等）、`qsxqj`、
  `xqjmcMap` 等。`kbList` 条目关键字段：`kcmc` 课程名、`kch_id` 课程号、
  `xm` 教师、`xqj` 星期(1-7)、`jc`/`jcs` 节次（如 `9-10节`）、`zcd` 周次
  （`第3周`、`4-19周`）、`cdmc` 教室、`jxbmc` 教学班、`kcxz` 课程性质、
  `xnm`/`xqm`、调课残留 `oldzc`/`oldjc`（位掩码）。
- 当前周次**接口不暴露**（`date`/`dateDigit` 等字段只是查询当天的全局戳，
  不是行课日期），必须由学期第一周周一（配置 `semester_start`）推算。
  2026-2027-1 的锚点：新生军训 9/9-9/22（校方新闻），军事理论排在 2-3 周
  （军训期间）、全校无任何课程排在第 1 周 → 第 1 周周一 = 2026-09-07，
  2026-10-20 为第 7 周。⚠️ 不要从某条 `zcd=第N周` 的记录反推“今天是第 N 周”
  ——那是行课计划，不是当前周。

## Ru41 模块（jwxt）

`src/modules/jwxt/`（`source.rs` CAS+课表客户端、`captcha.rs` 验证码求解、
`service.rs` 编排、`qq_command.rs` 查询命令）：轮询自动完成登录（会话复用 →
TGT 换 ST → 验证码自动求解），拉课表 → 落 `jwxt_timetable` → 与上次快照 diff，
调课/停课经默认出口提醒（同日 `jwxt_alert` 去重）→ 到达 `remind_hour` 后推送
明日课表（`daily:{date}` 每日一次）。QQ `/课表`、`/明日课表` 读缓存快照。
`jwxt login [--captcha]` 为人工兜底。

## 限制与注意事项

- **密码错误立即停手**：CAS 有失败计数与账号锁定（`PASSERROR`/`USERLOCK`），
  模块对密码类错误零重试；验证码识别失败才换图重试（单轮 ≤3 张）。
- 只读边界：只拉课表，不做选课、评教、成绩等写操作。
- 凭证只存 gitignored 本地配置或环境变量（`RU41_JWXT_USERNAME`/
  `RU41_JWXT_PASSWORD`）；session.json 等同登录态，不入库、不进 Vault。
