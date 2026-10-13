---
type: channel-adapter
status: implemented
channel: QQ 官方机器人开放平台
updated: 2026-10-13
tags: [Ru41, QQ, channel, gateway]
---

# QQ 渠道说明

Ru41 的第一个消息渠道，协议参照 XingtuBot-Spigot（Java）的 QQGatewayClient / QqOpenApiClient 用 Rust 重写。代码在 `src/ingress/qq/`：`gateway.rs`（WebSocket）、`api.rs`（REST）、`mod.rs`（组装与入队/通知逻辑）。

## 前置条件

- 在 QQ 开放平台创建机器人，拿到 AppID 与 ClientSecret。
- 在「权限管理」开启接收群聊@消息与 C2C 单聊消息（对应 intents `1<<25 | 1<<26`）；权限不足时网关以 4014/4914/4915 关闭码断开且不再重连。

## 协议要点

- **凭证**：`POST https://bots.qq.com/app/getAppAccessToken`，body `{"appId", "clientSecret"}`，返回 `access_token` 与 `expires_in`（字符串）。客户端缓存 token 并提前 60 秒重新申请；鉴权相关关闭码（4002/4004）会清缓存重新取。
- **WebSocket**：`GET https://api.sgroup.qq.com/gateway`（请求头 `Authorization: QQBot {token}`）拿 wss 地址 → 连接 → 收 HELLO（op 10，含 heartbeat_interval）→ 发 IDENTIFY（op 2，shard [0,1]）→ 心跳 op 1（间隔减 5 秒）保活 → DISPATCH（op 0）解析事件 → READY 记录 session_id，断线后带 session_id + 最后 seq 发 RESUME（op 6）；关闭码 4009 或 INVALID_SESSION（op 9, d=false）清会话强制重新 IDENTIFY。
- **事件**：`GROUP_AT_MESSAGE_CREATE`（`d.id/content/group_openid/author.member_openid`）、`C2C_MESSAGE_CREATE`（`d.author.user_openid`）。内容先去首尾空白，空消息不入队。
- **发消息**：`POST /v2/groups/{group_openid}/messages` 或 `/v2/users/{user_openid}/messages`，body `{content, msg_type: 0, msg_id, msg_seq}`。msg_type 0 为文本；同一 msg_id 多条回复需 msg_seq 自增（dispatcher 用内存计数器）。

## 与任务系统的衔接

- 入队键 `qq:msg:{msg_id}`，同一消息重复投递幂等；路由信息存 `tasks.notify_channel="qq"`、`notify_target="group:{openid}"` 或 `"user:{openid}"`、`notify_msg_id`。
- 通知循环每 2 秒轮询终态未通知任务，成功后 `mark_notified`（条件更新，并发下最多标记一次）。发送失败只记日志，下轮重试。
- 网关与 Worker 是独立进程；`qq run` 只负责收发，执行仍由 `worker` 完成。

## 限制

- 被动回复依赖 msg_id，QQ 侧约 5 分钟有效；超时回复会失败，结果仍可在 `task show` 查看。
- 主动消息（不带 msg_id）受平台配额与频控限制，当前仅 `qq send` 手动测试使用；主动简报类推送需另行评估额度。
- 当前只收发文本；图片、语音（ASR）、markdown 模板与按钮交互后续按需要扩展。
- 单实例运行：同一机器人多处启动会互相踢下线（关闭码 4004）。
