#!/usr/bin/env python3
"""把浏览器渲染出来的 HTML 压成一份紧凑快照，供 agent 直接读。

为什么需要它：浏览器返回的是整页 HTML（X 一页 ~185KB ≈ 4.6 万 token），
而真正有信息量的正文只有几 KB。让原始 HTML 进上下文既贵又干扰模型；
这里在**进上下文之前**先压一遍，模型只看到结构化的结果。

用法（管道，别让 HTML 进上下文）：

    curl -s -X POST http://ru41-browser:3000/content \
      -H 'Content-Type: application/json' \
      -d '{"url":"https://example.com","gotoOptions":{"waitUntil":"networkidle2"}}' \
      | python3 snapshot.py --url https://example.com

参数：
    --url URL        页面地址（有些站点渲染后地址会变，显式传更准）
    --max-chars N   正文最大字符数，默认 6000
    --full          不截断正文（默认截断；先看结构，需要细节再重跑）
"""

from __future__ import annotations

import argparse
import re
import sys

try:
    from bs4 import BeautifulSoup
except ImportError:  # 没 bs4 就退回正则，够用但不精细
    BeautifulSoup = None  # type: ignore[assignment]

# 这些容器里的内容多半是导航/推荐，不是正文，先扔掉。
DROP_TAGS = ("script", "style", "noscript", "svg", "canvas", "iframe", "form")
# 正文候选容器（按优先级）。
MAIN_SELECTORS = (
    "article",
    "main",
    '[role="main"]',
    "#main",
    ".article-content",
    ".RichText",
)


def squeeze(text: str) -> str:
    """压空白：连续空行收成一行，行内空白收成一个空格。"""
    text = re.sub(r"[ \t\r\f\v]+", " ", text)
    text = re.sub(r"\n\s*\n\s*(\n\s*)+", "\n\n", text)
    return text.strip()


def fallback_extract(html: str) -> tuple[str, str]:
    """没有 bs4 时的粗提取：去标签取文本。"""
    cleaned = re.sub(r"(?is)<(script|style|noscript)[^>]*>.*?</\1>", " ", html)
    cleaned = re.sub(r"(?s)<[^>]+>", "\n", cleaned)
    return "", squeeze(cleaned)


def extract(html: str) -> dict:
    if BeautifulSoup is None:
        title, text = fallback_extract(html)
        return {"title": title, "headings": [], "text": text, "links": [], "controls": []}

    soup = BeautifulSoup(html, "html.parser")
    for tag in soup(list(DROP_TAGS)):
        tag.decompose()

    title = (soup.title.get_text(strip=True) if soup.title else "") or ""

    headings = []
    for level in ("h1", "h2", "h3"):
        for node in soup.find_all(level):
            text = node.get_text(" ", strip=True)
            if text:
                headings.append(f"{level.upper()} {text}")

    # 正文：优先取语义化容器，取不到再退回 body。
    main = None
    for selector in MAIN_SELECTORS:
        found = soup.select_one(selector)
        if found and len(found.get_text(strip=True)) > 200:
            main = found
            break
    body = main or soup.body or soup
    text = squeeze(body.get_text("\n", strip=True))

    links = []
    for a in soup.find_all("a", href=True):
        label = a.get_text(" ", strip=True)
        href = a["href"]
        if label and len(label) > 1 and not href.startswith("javascript:"):
            links.append(f"- [{label[:80]}]({href[:200]})")
        if len(links) >= 40:
            break

    controls = []
    for node in soup.find_all(["button", "input", "select", "textarea"]):
        kind = node.name
        label = (
            node.get("aria-label")
            or node.get("placeholder")
            or node.get("value")
            or node.get_text(" ", strip=True)
            or ""
        )
        label = squeeze(str(label))[:60]
        if label:
            controls.append(f"- {kind}: {label}")
        if len(controls) >= 30:
            break

    return {
        "title": title,
        "headings": headings,
        "text": text,
        "links": links,
        "controls": controls,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="")
    parser.add_argument("--max-chars", type=int, default=6000)
    parser.add_argument("--full", action="store_true")
    args = parser.parse_args()

    html = sys.stdin.read()
    if not html.strip():
        print("（空响应：页面可能需要登录、被反爬拦截，或 URL 错了）")
        return 0

    data = extract(html)
    out = []
    if data["title"]:
        out.append(f"标题：{data['title']}")
    if args.url:
        out.append(f"来源：{args.url}")
    out.append(f"原始 HTML：{len(html)} 字节 → 快照 {len(data['text'])} 字符")

    if data["headings"]:
        out.append("\n## 页面结构")
        out.extend(data["headings"][:30])

    text = data["text"]
    truncated = False
    if not args.full and len(text) > args.max_chars:
        text = text[: args.max_chars]
        truncated = True
    out.append("\n## 正文")
    out.append(text if text else "（没抽到正文：可能是登录墙/纯前端渲染/反爬）")

    if data["controls"]:
        out.append("\n## 可交互元素")
        out.extend(data["controls"])

    if data["links"]:
        out.append("\n## 链接")
        out.extend(data["links"][:25])

    if truncated:
        out.append(f"\n（正文已截断到 {args.max_chars} 字符；要全文加 --full 重跑）")

    print("\n".join(out))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
