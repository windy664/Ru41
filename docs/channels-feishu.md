---
type: channel-adapter
status: implemented
channel: 飞书自定义群机器人 webhook
updated: 2026-10-14
tags: [Ru41, 飞书, channel, notify]
---

# 飞书渠道说明

第二个通知渠道，只负责把终态结果**主动推送**到群，无双向交互（双向需要应用机器人，后续单独接入）。实现为 `src/ingress/feishu.rs` 的 `FeishuWebhookNotifier`（`NotifyChannel` trait），投递仍由 `runtime::dispatcher` 统一轮询与重试。

## 前置条件

- 在飞书群「设置 → 机器人」添加自定义机器人，拿到 webhook URL（`https://open.feishu.cn/open-apis/bot/v2/hook/{token}`）。
- 安全设置选择**自定义关键词**或 **IP 白名单**；签名验证暂未支持，开启签名的机器人会拒绝请求（code 19021）。

## 配置与运行

- webhook URL 含 token，按密钥处理：环境变量 `RU41_FEISHU_WEBHOOK_URL` 或配置 `[channel.feishu] webhook_url`（环境变量优先）。
- `ru41 qq run` 的派发循环会自动带上已配置的飞书渠道；不跑 QQ 的部署用 `ru41 notify` 单独运行派发。

## 任务如何路由到飞书

```toml
[notify]
channel = "feishu"
```

```bash
cargo run --locked -- task add --key report:2026-W42 --prompt "生成本周复盘" --notify
```

- 入口只声明「要通知」，装配层按 `[notify] channel` 解析出具体路由——渠道选择集中在这一处。
- 任务的 `notify_channel` 记为 `feishu`，`notify_target` 只存占位值 `default`，webhook URL 不落库；发送时由 notifier 解析 `default` 为配置的 URL。
- `notify_target` 也接受完整 webhook URL（向非默认机器人投递时使用）。
- 终态后由 dispatcher 推送：成功发结果摘要，失败发错误原因；发送失败按退避重试（delivery 记录，最多约 1 小时间隔）。

## 限制

- 只发文本；无卡片、按钮与回复交互。
- 推送对象固定为机器人所在的群，不能定向到个人。
- 主动推送不走 msg_id，不受 QQ 那样的 5 分钟被动回复窗口限制，但受飞书侧频控（每机器人每分钟约 100 条）约束。
