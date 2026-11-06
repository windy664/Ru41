# 浏览器能力：让 agent 操作真页面

Ru41 的 agent（pi）默认**没有**浏览器：它能跑 bash（curl/wget）、能读文件，
但看不到 JS 渲染后的页面，也拿不到截图。这份文档讲两件事：

1. 现状：没有浏览器时怎么办（`/content` 快照管道）；
2. 进阶：把 [pi-chrome-use](https://github.com/citrolabs/pi-chrome-use) 装上，
   让 agent 直接操作真实 Chromium（DOM、JS、截图）。

## 1. 现状：快照管道（默认够用）

共享无头浏览器 `ru41-browser`（browserless/chromium）提供 HTTP 接口：

```bash
curl -s http://ru41-browser:3000/content?url=<URL> | \
  python3 /usr/share/ru41/modules/shared-tools/skills/web-browser/snapshot.py --url <URL>
```

`snapshot.py` 把整页 HTML 压成紧凑快照（标题 / 正文 / 可交互元素 / 链接），
上下文体积降约 8 倍。**绝大多数「读网页」的任务用这条就够**。

已知限制：X（x.com）、知乎等站点对无头浏览器直接 403，遇到就换信息源，
不要反复重试。

## 2. 进阶：pi-chrome-use（真浏览器 + 截图）

[pi-chrome-use](https://github.com/citrolabs/pi-chrome-use) 是 pi 的官方生态
扩展：给 agent 一个 `browser_execute` 工具，经 CDP 驱动真实 Chromium——
跑 JS、点按钮、填表单、看 DOM，并把**截图作为图片结果返回给模型**
（我们的 MiniMax-M3 支持图片输入）。

实测（2026-11-06）：pi 加载该扩展后，用 `ws://ru41-browser:3000/chromium`
连上共享浏览器，打开 example.com 拿到标题，并成功返回 PNG 截图。

### 2.1 怎么接上

**镜像层**（已做）：`Dockerfile` 里 `pi install npm:pi-chrome-use@1.1.1`，
扩展落在

```
/home/node/.pi/agent/npm/node_modules/pi-chrome-use/extensions/browser-execute.ts
```

**配置层**（`[agent.pi]` 段，两个新字段）：

```toml
[agent.pi]
tools = "read,write,edit,bash,browser_execute"
# 只加载这里写下的扩展；Pi 的扩展发现始终是关的（--no-extensions 保留），
# 装了扩展包也不会自动多出工具。
extensions = ["/home/node/.pi/agent/npm/node_modules/pi-chrome-use/extensions/browser-execute.ts"]

[agent.pi.extra_env]
# 扩展靠这个环境变量找 CDP 端点（值只进子进程环境，不写日志）
BU_CDP_WS = "ws://ru41-browser:3000/chromium"
```

`extra_env` 是通用的子进程环境变量注入，别的扩展要读配置也用它。

改完要重启跑 agent 的容器（worker）才生效。

### 2.2 CDP 端点

browserless/chromium v2 的 CDP WebSocket 在 `/chromium`（不是 `/webkit`，
那是旧 chrome 镜像的路径）：

| 端点 | 用途 |
|---|---|
| `ws://ru41-browser:3000/chromium` | CDP（给 pi-chrome-use 用） |
| `http://ru41-browser:3000/content` | 渲染后 HTML（快照管道用） |
| `http://ru41-browser:3000/pressure` | 并发/排队状态 |

实测 `Browser.getVersion` 返回 Chrome/156 headless，协议版本 1.3。

### 2.3 边界与代价

- **会话即占用**：pi-chrome-use 在一次 pi 会话里复用同一个 CDP 连接，
  也就是**整个任务期间占着一个 chromium 槽位**（browserless
  `maxConcurrent=10`）。任务并发一高，共享浏览器会被占满，页面监控那些
  依赖渲染的附属就会排队。必要时把 `maxConcurrent` 调大。
- **CDP = 浏览器完全控制权**：连上的浏览器能访问的，agent 都能访问
  （容器网络内的其他服务也在射程内）。我们默认不给 agent 开这个工具，
  要开就明确开在 `[agent.pi].tools` 里。
- **截图进上下文**：截图是图片，会占 token；一次几 MB 的 PNG 换算下来不便宜。
  任务不需要看画面时，让 agent 只取 DOM/文本。

## 3. 该用哪个

| 需求 | 选择 |
|---|---|
| 读一篇网页的正文 | 快照管道（`/content` + snapshot.py） |
| 摘要一串 RSS/新闻 | 快照管道 |
| 需要登录态、点按钮、填表 | pi-chrome-use |
| 需要「用户会看到什么」的证据 | pi-chrome-use（截图） |
| 平台 403 / 反爬 | 先换信息源，别硬刚 |

## 4. 关联文档

- 共享浏览器与 compose：[deploy/README.md](../deploy/README.md)
- 技能包与快照管道：`src/modules/shared-tools/skills/web-browser/SKILL.md`
- 底座配置全集：[usage.md](usage.md)
