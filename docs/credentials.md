# 凭证与密钥配置总览

各类 cookie / token / key / 密码的注入路径、存放位置与更换方法。各源的详细
配置见对应文档（`sources-*.md` / `channels-*.md`），本页只讲凭证体系本身。

## 五条注入路径

| 路径 | 存什么 | 特点 |
|---|---|---|
| 配置文件 `ru41.toml` | 账密、token 等站点凭证 | 明文；`ru41.example.toml` 是带注释样例，真实文件不入 git |
| 环境变量 | API key、cookie 等高敏凭证 | 配置里只存变量名（`api_key_env` 间接引用），值不落盘到配置 |
| 会话持久化文件 | 登录后的 cookie/TGT | 不是密钥是登录态；避免每轮重登触发风控；路径可配，默认 `.ru41/` |
| sidecar 自管 | 爬虫自己的浏览器 profile/cookie | 主程序不碰，sidecar 只通过产物文件（如 `boss_jobs.json`）交换数据 |
| git credential helper | Vault 仓库推送凭证 | 标准 git 机制（`git-credentials`），ru41 配置中不出现 |

配置取值优先级：CLI > 环境变量 > 配置文件 > 默认值。

## 凭证清单

| 凭证 | 配置位置 | 环境变量（推荐） | 用途 |
|---|---|---|---|
| GLM API key | 环境变量（`[agent.codex.provider] api_key_env` 指定变量名） | `ZHIPU_API_KEY`（名字可自定） | Agent 底座模型调用 |
| QQ AppID/Secret | `[channel.qq]` | `RU41_QQ_APP_ID` / `RU41_QQ_APP_SECRET` | QQ 机器人鉴权 |
| QQ 绑定口令 | `[channel.qq] bind_secret` | `RU41_QQ_BIND_SECRET` | 身份绑定命令开关 |
| 飞书 webhook | `[channel.feishu]` | `RU41_FEISHU_WEBHOOK_URL` | 通知出口（URL 含 token） |
| 学习通账密 | `[sources.chaoxing]` | `RU41_XXT_PHONE` / `RU41_XXT_PASSWORD` | 协议登录 → cookie |
| 教务账密 | `[sources.jwxt]` | `RU41_JWXT_USERNAME` / `RU41_JWXT_PASSWORD` | CAS 登录 → TGT |
| 校园卡 token | `[sources.ecard]` | `RU41_ECARD_TOKEN` | 水电查询 |
| 雨课堂 openid | `[sources.teachermate]` | `RU41_WZJ_OPENID` | 作业轮询（人工投喂） |
| 恒源云 cookie | 仅环境变量 | `RU41_COMPSHARE_COOKIE` | GPU 库存监控 |
| HTTP API token | `[api]` | `RU41_API_TOKEN` | `ingress/http` 鉴权 |
| Vault git 凭证 | git credential helper | （git 标准） | Obsidian Vault 推送 |

## 会话文件（登录态，非密钥）

| 文件 | 归属 | 说明 |
|---|---|---|
| `.ru41/chaoxing/cookies.json` | 学习通 | 账密登录一次后复用；`cookies.qr.json` 是二维码签到独立会话 |
| `.ru41/jwxt/session.json` | 教务 | TGT + cookie 持久化 |
| `.ru41/teachermate/openid` | 雨课堂 | openid 落盘 |

会话失效时自动走账密重登并回写；`.ru41/` 目录必须 gitignore。

## 换 key 的操作

- **GLM key**：改 worker 容器的 `ZHIPU_API_KEY` 环境变量（`docker rm -f` 后带新 env 重建，或 compose 改 env）→ 重启容器。配置文件不用动。
- **校园系统改密**：改配置文件对应 `[sources.*]` 的 password（或其环境变量）→ 重启该源容器；首次轮询会自动用新账密换新会话。
- **QQ/飞书**：优先环境变量；`ru41 qq onboard` 扫码流程会把凭证原子写入本地配置（0600），Secret 不进日志。

## 安全原则

1. 仓库内零真实密钥：样例与测试全用假值，真实配置只存在于部署机。
2. 密钥类凭证优先环境变量；配置文件里的字段一律有对应 `RU41_*` 替代。
3. Worker `sanitize` 开启时，任务输出经脱敏才进通知渠道（手机号等打码）。
4. 敏感卷按需挂载：各 sidecar 的 cookie/profile 不共享给主程序容器。
5. 日志与事件流不落密钥：事件不包含 prompt、结果正文与凭证字段。
