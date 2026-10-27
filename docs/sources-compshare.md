---
type: source-adapter
status: implemented
source: 恒源云 light-gpu 控制台 API
updated: 2026-10-27
tags: [Ru41, Compshare, GPU, source]
---

# 恒源云 GPU 库存监控

定时调用控制台创建页使用的 `CheckCompShareResourceCapacity`，监控指定镜像、地域、可用区和 GPU 型号；只要响应 `Specs[].ResourceEnough` 有 true，就视为可创建并推送通知。与网页监控不同，不看 HTML 文本变化，也不会监控其他页面。

## 默认目标

当前默认跟随用户的 P40 需求：

- 镜像：`compshareImage-1t1vuerl80np`（cuda132_torch2130_py312）
- 地域：华北二A（`cn-wlcb` / `cn-wlcb-01`）
- GPU：`P40`
- 系统盘：100 GB
- 计费：按量后付费
- 间隔：60 秒（调度核心最低仍保护到 30 秒）

## 配置（ru41.toml）

`Cookie` 是完整登录态，必须从环境变量 `RU41_COMPSHARE_COOKIE` 注入；不要写进仓库、Vault 或日志。CSRF token 从 cookie 中的 `U_CSRF_TOKEN` 读取，`_user` 从 `U_USER_EMAIL` 读取。

```toml
[notify]
channel = "qq"

[channel.qq]
default_notify_target = "user:<openid>"

[sources.compshare]
project_id = "org-..."
image_id = "compshareImage-1t1vuerl80np"
region = "cn-wlcb"
zone = "cn-wlcb-01"
gpu_type = "P40"
disk_size_gb = 100
interval_secs = 60
notify = true
```

## 运行

```bash
export RU41_COMPSHARE_COOKIE='完整的 .compshare.cn Cookie'
cargo run --locked -- compshare poll   # 单轮调试
cargo run --locked -- compshare run    # 常驻周期轮询
cargo run --locked -- schedule run compshare
```

## 幂等与通知路径

- 状态记录：`compshare_state`，键为 `image:region:zone:gpu`。
- 首轮有货、`无货 → 有货` 时通知；持续有货或持续无货不重复打扰。
- 通知失败时不写入新状态，下一轮重试；QQ 主动消息配额有限，因此不在每轮发送。
- 会话失效记录为 `auth_expired`，不改变库存状态；`compshare_auth_alert` 按天最多提醒一次。
- 业务模块只拿 `NotifyChannel + target`；默认出口由 `[notify]` 和装配层决定。

## 会话更新与隐私边界

Cookie 中的 JWT 会过期。失效后重新登录 `console.compshare.cn`，更新运行环境中的 `RU41_COMPSHARE_COOKIE` 并重启 `compshare` 进程即可。数据库只保存库存状态、规格和时间戳，不保存 Cookie、CSRF 或 JWT。

本模块只查询和提醒，不自动下单，不触发创建实例。
