---
name: web-browser
description: 用共享无头浏览器渲染 JS 重度页面（X、知乎、SPA 站点）抓取内容。普通静态页面优先 curl/python requests 直接抓，不要动用浏览器。
---

# 共享无头浏览器（browserless）

worker 容器里没有浏览器；浏览器是独立容器（ru41-browser）。
两条路：HTTP API（下面的 curl，快、便宜）和 **browser_execute**（CDP，能交互、能截图、图直接进上下文）。

## 何时用

- 页面需要 JS 渲染才有内容（多数 SPA、无限滚动列表）
- 静态页面**不要**用浏览器：curl / python requests 更快更省
- **已知抓不到的**：X（x.com）与知乎当前对这台机器的出口 IP 直接 403（"Access denied" / "请求存在异常"）。
  遇到 403 不要反复重试——换信息源（官方 API、RSS、镜像站）更有效。

## 铁律：原始 HTML 不要进上下文

浏览器返回的是整页 HTML（X 一页 185KB ≈ 4.6 万 token），正文实际只有几 KB。
**必须**在进上下文之前先压成快照，否则白白烧 token 且干扰判断。

```bash
curl -s -X POST http://ru41-browser:3000/content \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","gotoOptions":{"waitUntil":"networkidle2"}}' \
  | python3 /usr/share/ru41/modules/shared-tools/skills/web-browser/snapshot.py \
      --url https://example.com
```

快照脚本会输出：标题 / 来源 / 原始字节数 → 快照字符数 / 页面结构（各级标题）/ 正文 /
可交互元素（按钮、输入框）/ 链接。默认正文截断到 6000 字符，要全文加 `--full` 重跑。

**不要**把 `curl` 的输出直接打印出来看——那才是 18 万 token 的来源。

## 截图：优先 browser_execute

需要截图、或者要跟页面交互（点按钮、填表、读 DOM），用 **browser_execute 工具**
（pi-chrome-use 扩展，已装好）：截图会**作为图片直接进你的上下文**——你能真的看见
页面，不用猜；文件落在运行目录里，回复用户时系统会自动把它附上（也可以在最终
JSON 的 media 数组里显式指定文件）。

只有"要批量取图、又不想让图进上下文"这种场景才用下面的 curl 办法：

```bash
curl -s -X POST http://ru41-browser:3000/screenshot \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","options":{"type":"jpeg","quality":80,"fullPage":true}}' \
  -o page.jpg
```

curl 取到的截图是二进制文件，**别读进上下文**；只报文件名与路径即可。

## 注意

- 浏览器容器已接代理，国际站点直接可访问；国内站点走直连规则
- 单页渲染要几秒到十几秒，批量抓取注意节奏（容器允许 10 并发，别一次打满）
- 不访问需要登录态的私人站点；不绕验证码
