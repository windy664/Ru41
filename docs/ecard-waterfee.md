---
type: source-adapter
status: reverse-engineered
source: G 校校园卡（新中新 ecard 平台）
updated: 2026-10-14
tags: [Ru41, ecard, 水电费, source]
---

# 校园卡水电费接口说明

对「G 校校园卡」微信小程序（AppID 略）的逆向结果，对应设计文档 3.2 水电费模块。**本文档只记录协议；token、房间号等个人参数存本地配置，不入库。**

## 平台与认证

- 后端：新中新 ecard 平台（厂商 greatge），API 基址 `https://ecarduser.<school>.edu.cn/`（走本地配置 `sources.ecard.base_url`，不写入代码）；统一认证走校方 CAS（联奕）（联奕 CAS）。
- 小程序登录链路：`wx.login` 得临时 code → `POST /user/routine/routine-login {code, openid, unionid, sno}` → 返回 `token`。**token 长效**：小程序跨会话复用本地存储的 token，routine-login 请求本身就携带旧 token。过期后只能重新走 wx.login code（需微信环境），无法纯 API 续期——Ru41 侧应在查询失败时提醒重新抓一次。
- 请求签名（登录类接口需要）：`sign = MD5(按 key 排序的 k=v& 拼接 + "greatge").toUpperCase()`，`token` 与 `sign` 本身不参与签名；公共字段 `from=wxminiprogram`、`wxRequest="wxRequest"` 参与签名。**水电查询接口不校验 sign**（抓包请求中无 sign 字段），只凭 `token`。
- 账号体系：登录方式 `SYSTEM_ACCOUNT`（学工号），另有 `phonePwdLogin`（密码 MD5[8:24]）、`sendVerifyCode` + `phoneVerifyCodeLogin`（短信验证码）等端点，但均要求先绑定 openid，纯 API 无法完成首次登录。

## 水电接口（H5 同源，POST 表单）

| 接口 | 参数 | 返回 |
|---|---|---|
| `/powerfee/getBalance` | implType, schoolAreaNo, buildingNo, roomNum, from, token | `obj.powerBalance`（度）、`waterBalance`（吨）、`formatPowerBalanceStr`、`formatWaterBalanceStr`、校区/楼栋/房间名 |
| `/powerfee/getDailyDetails` | roomNum, lastDate(YYYY-MM), type(1=水 2=电), implType, schoolAreaNo, pageNum, pageSize, token, from | `dailyDetailsInfos[]`：`dailyUsed`（当日用量）、`leftUsed`（当日剩余）、`dateTime` |
| `/waterfee/getRoomInfo` | implType, token | 校区→楼栋→房间三级列表（换宿舍时重新发现房间用） |
| `/waterfee/getBalance` | implType, roomNum, buildingNo, token | 水费余额（老水控体系；本平台水电都在 powerfee 返回） |
| `/waterfee/checkAccount` | sno, implType, token | 水控类型判定；`MINGHANBLUETOOTH` 会跳「铭汉水控」另一小程序 |

本校实测：水、电余额统一由 `powerfee/getBalance` 返回（implType=CGCOMMON1111），`getDailyDetails` 取日用量（含充值造成的负用量，可据此区分充值与消耗）。

## Ru41 模块（已实现）

`src/modules/ecard/`（`source.rs` 客户端、`service.rs` 编排、`qq_command.rs` QQ 查询）：`ecard poll/run` 查询余额 → 读数以 `ecard_reading` 落 records（同日覆盖、跨日成历史，余额回升即充值）→ 阈值判断（纯 Rust，不经 Agent，符合「金额由程序计算」）→ 低余额经 `[notify]` 默认出口提醒，同日 `ecard_alert` 去重不重复打扰。token 与房间参数走 `[sources.ecard]` 本地配置或 `RU41_ECARD_TOKEN`。

QQ 提供两个入口：`/水电` 立即返回最近一次读数；自然语言（如“电费还剩多少，顺便分析”）则由 ecard handler 先校验身份绑定和单聊上下文，再读取最近 30 条缓存记录、注入 Agent 任务并由 `skills/ecard-utility/SKILL.md` 分析。Agent 不直接接触数据库或校园卡接口，也不会触发轮询、阈值计算或充值，避免自然语言任务绕过隐私边界。

## 限制与注意事项

- **WAF 限流**：单 IP 突发请求会被断连（SSL reset），间隔数秒恢复。采集节奏应≤1 次查询/天·房间，失败退避重试。
- token 有效期未实测（抓包当天有效）；过期表现为查询返回非 ret:true 或跳转登录，届时需重新抓包（mitmproxy + PC 微信手动代理，全流程约 5 分钟）。
- 充值下单接口（`/user/account/chargePay` 等）已定位但未分析；首版只做只读监控，不做自动充值。
- 抓包路径备忘：mitmdump（127.0.0.1:8080）+ PC 微信设置手动代理 + 打开小程序点水费页；CA 已装入用户 NSS DB（`~/.pki/nssdb`，certutil 免 root）。
