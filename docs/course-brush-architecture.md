# 自动刷课：ru41 怎么管这件事（course-brush 插件）

> 2026-11-24 落地。起因是用户一句「我们要的是自动刷课哦 ru41 得有这个功能」——
> 原来刷课完全是 ru41 之外的东西：生产机上的 systemd timer + sidecar 的 Python 脚本，
> ru41 既不调度、也不知道刷到哪了。

## 为什么刷课没有整个写进 ru41

平台的「资源已完成」标记，是**浏览器里的前端 JS** 上报的，请求体还是加密的
（`secretStr`）。没有真实浏览器点，就点不亮那个标记；伪造上报更是不可能。

- ru41 的容器里**没有浏览器**，也没挂 `docker.sock`；
- 在 Rust 里重写一套浏览逻辑又慢又脆（页面结构一变就坏）。

所以分两层，各干各擅长的事：

```
ru41-course-brush 容器（ru41 插件 course-brush）
   │  HTTP：ru41-net 内网，容器名直连
   ▼
ru41-sidecar 容器（zhihuishu-sidecar:local，带 Playwright + Xvfb）
   └─ scripts/serve.py：POST /brush 真的去刷
```

| 层 | 做 | 不做 |
|---|---|---|
| **ru41 插件 course-brush** | 什么时候刷、汇报、通知、对外 CLI | 不碰浏览器、不解释平台语义 |
| **sidecar serve.py** | 开浏览器点视频/浏览文档，让平台自己上报完成 | 不挑课（挑课是 `plan_targets.py` 的事）、不通知 |

## 目录树

```
plugins/course-brush/
├── Cargo.toml                 # 插件 crate（cdylib）
├── config.example.toml        # base_url / 超时 / 间隔 / 预算 / 通知开关
├── src/
│   ├── lib.rs                 # DEF：调度清单 + tracker + CLI 钩子
│   ├── config.rs              # [course-brush] 配置段
│   ├── client.rs              # 跟 sidecar 说话（**必须 no_proxy**，见下）
│   ├── service.rs             # 一轮 poll：问该不该刷 → 刷 → 渲染 → 通知
│   └── cli.rs                 # `ru41 course-brush status|brush|refresh`
└── tests/course_brush.rs      # 5 条集成测试（mock sidecar）

sidecar/zhihuishu-sidecar/
├── scripts/serve.py           # HTTP 服务：/health /status /brush /targets/refresh
└── deploy/ru41-sidecar-serve.service   # 用户级 systemd：起 ru41-sidecar 常驻容器
```

## 一轮 poll 长什么样

1. `GET /status` → sidecar 答「今天该不该刷 + 为什么」；
2. **不该刷** → 出报告，**不发通知**（日常状态是噪音，一天一条「今天不刷」没人爱看）；
3. **该刷** → `POST /brush`（同步，sidecar 跑完整个 `brush_courses.py` 才返回，
   可能几十分钟到几小时，所以单独一个超时）→ 拿回结果 → 渲染报告 → **发通知**
   （成或败都发，失败尤其要发）。

## 「今天该不该刷」的判定为什么放在 sidecar 那边

**容器时区是 UTC，宿主和 ru41 是 CST。** 两边各算各的「今天」，在 CST 晚上
八点前后就会错开一天。

所以判定统一在 sidecar 侧做：`brush_log.generated_at` 是容器本地时间（UTC），
解析成 UTC → 换算到 CST → 再比日期和开工点（默认 19:00 CST，`SIDECAR_DAILY_HOUR` 可调）。

判据三条：
1. 正在刷 → 不该（上轮没结束）；
2. 最后一轮就是今天 → 不该；
3. 现在还没到开工点 → 不该。

## 踩过的坑

- **代理解析不了容器名**：worker 容器注入了 `HTTP_PROXY`（指向 mihomo），
  而 `NO_PROXY` 只列了 `localhost/127.0.0.1/ru41-browser/172.17.0.1` ——
  请求 `http://ru41-sidecar:9100` 被发给代理，拿到**空响应**，报
  「EOF while parsing a value」。按 `src/adapters/sources/competition/xfyun.rs`
  里 ru41-browser 的同一个坑处理：单独建 `no_proxy` 客户端。
- **`poll|run` 这两个词被宿主留给 tracker**（`poll`=跑一轮，`run`=起常驻循环，
  见 `src/main.rs` 的 `Command::Tracker` 分支），插件 CLI 钩子拿不到 ——
  所以「强制刷一轮」叫 `brush`，不叫 `run`。
- **插件目录名带连字符时收集不到 .so**：cargo 把 crate 名的连字符换成下划线
  （`ru41-plugin-course-brush` → `libru41_plugin_course_brush.so`），
  而 `deploy/build-fast.sh` 按目录名找连字符版 → FATAL。已改成两种都试。

## 相关文档

- `sidecar/zhihuishu-sidecar/README.md` —— ai-smart 的三种页面形态（视频课 / 图谱课 / 内容未发布）
- `docs/campusrun-architecture.md` —— 校园跑插件层的职责边界（插件只转述后端）
