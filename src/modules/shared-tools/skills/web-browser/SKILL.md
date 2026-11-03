---
name: web-browser
description: 用共享无头浏览器渲染 JS 重度页面（X、知乎、SPA 站点）抓取内容。普通静态页面优先 curl/python requests 直接抓，不要动用浏览器。
---

# 共享无头浏览器（browserless）

worker 容器里没有浏览器；浏览器是独立容器，通过 HTTP API 使用。

## 何时用

- 页面需要 JS 渲染才有内容（X/Twitter、知乎、多数 SPA、无限滚动列表）
- 静态页面**不要**用浏览器：curl / python requests 更快更省

## 用法（curl 即可）

渲染并取完整 HTML（等网络静默）：

```bash
curl -s -X POST http://ru41-browser:3000/content \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","gotoOptions":{"waitUntil":"networkidle2"}}'
```

返回渲染后的 HTML 文本，用 python+bs4 提取正文。

截图（返回二进制，保存为文件）：

```bash
curl -s -X POST http://ru41-browser:3000/screenshot \
  -H 'Content-Type: application/json' \
  -d '{"url":"https://example.com","options":{"type":"jpeg","quality":80,"fullPage":true}}' \
  -o page.jpg
```

## 注意

- 浏览器容器已接代理，国际站点直接可访问；国内站点走直连规则
- 单页渲染要几秒到十几秒，批量抓取注意节奏，别并发打满
- 不访问需要登录态的私人站点；不绕验证码
- API 无鉴权，仅限内网容器使用，不要把 3000 端口映射到宿主机
