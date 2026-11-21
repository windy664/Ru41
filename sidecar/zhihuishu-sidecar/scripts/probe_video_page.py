"""探针: 摸清新版 AI 课「课程视频」页的 DOM —— Autovisor 认不认得。

背景(2026-11-21): Autovisor 用 4 类 CSS 选择器认课程目录
(modules/lesson_navigation.py):
    wisdom = .child-info.hasvideo / fusion = .chapter-item
    hike   = .file-item         / legacy = .clearfix.video
在 ai-smart 的 courseVideoShare 页上**全部命中 0** → detect_catalog 抛
RuntimeError → 该门课失败 → 整轮 break。

所以要么:
  a) 页面还没渲染完/要先展开章节 → 多等一会儿就好了(那就调等待策略);
  b) 智慧树改了 DOM → 上游需要打补丁(patches/ 里已有先例)。

本探针把课时列表附近的 class 名打出来, 人工判断到底是哪种。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_video_page.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import re
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

CATALOGS = {
    "wisdom": ".child-info.hasvideo",
    "fusion": ".chapter-item",
    "hike": ".file-item",
    "legacy": ".clearfix.video",
}


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            print(f"URL: {page.url}", flush=True)
            print(f"标题: {await page.title()}", flush=True)

            for name, sel in CATALOGS.items():
                try:
                    n = await page.locator(sel).count()
                except Exception:
                    n = "err"
                print(f"  {name:<10} {sel:<26} 命中 {n}", flush=True)

            # 课时列表: 找带 title 属性的元素, 往上爬几层看容器 class
            info = await page.evaluate(
                """() => {
                    const out = {count: 0, samples: [], classes: {}};
                    const els = Array.from(document.querySelectorAll('[title]'));
                    out.count = els.length;
                    for (const el of els.slice(0, 40)) {
                        const t = (el.getAttribute('title') || '').trim();
                        if (!t) continue;
                        let node = el, chain = [];
                        for (let i = 0; i < 4 && node; i++) {
                            chain.push(node.className || node.tagName);
                            node = node.parentElement;
                        }
                        out.samples.push({title: t.slice(0, 24), chain});
                        for (const c of chain) {
                            if (typeof c === 'string' && c) {
                                out.classes[c] = (out.classes[c] || 0) + 1;
                            }
                        }
                    }
                    out.videos = document.querySelectorAll('video').length;
                    out.topClasses = Array.from(document.querySelectorAll('body *'))
                        .map(e => e.className)
                        .filter(c => typeof c === 'string' && c.trim())
                        .join(' ')
                        .split(/\\s+/)
                        .filter(Boolean);
                    return out;
                }"""
            )
            print(f"\n带 title 属性的元素: {info['count']} 个; <video>: {info['videos']} 个",
                  flush=True)
            for s in info["samples"][:12]:
                print(f"  {s['title']!r:<30} 祖先链: {' ← '.join(s['chain'])}", flush=True)
            top = sorted(set(info["topClasses"]), key=lambda c: -info["topClasses"].count(c))
            print(f"\n最常见的 class(前 30): {top[:30]}", flush=True)
            with open("/app/data/probe_video_dom.json", "w", encoding="utf-8") as fh:
                import json

                json.dump(info, fh, ensure_ascii=False, indent=1)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
