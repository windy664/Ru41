"""探针: 数一数课程页里 Autovisor 那 4 类目录选择器各命中多少个.

为什么问这个(2026-11-21): Autovisor 认课**不靠 URL 域名, 靠 DOM 选择器**
(modules/lesson_navigation.py: detect_catalog):
    wisdom  = .child-info.hasvideo          ← 新版 AI 课(智慧树 ai-smart)
    fusion  = .chapter-item                  ← 融合课(fusioncourseh5)
    hike    = .file-item                     ← hike.zhihuishu.com
    legacy  = .clearfix.video                ← 旧版
四个都不命中 → detect_catalog 抛 RuntimeError → 该门课失败 → **整轮 break**
(Autovisor.py: 一门 FAILED 就 break 掉后面的课)。

所以「这门课 Autovisor 刷不刷得了」= 「页面上有没有它的目录选择器」。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_catalog.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
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
    "wisdom(新版AI课)": ".child-info.hasvideo",
    "fusion(融合课)": ".chapter-item",
    "hike": ".file-item",
    "legacy(旧版)": ".clearfix.video",
    # 顺带看看别的可能
    "any-child-info": ".child-info",
    "video-tag": "video",
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
                except Exception as exc:
                    n = f"err {str(exc)[:60]}"
                print(f"  {name:<18} {sel:<26} 命中 {n}", flush=True)
            # 目录项的标题长啥样(判是不是真课时)
            for sel in (".child-name", ".item-name", "#lessonOrder", "span[title]"):
                try:
                    n = await page.locator(sel).count()
                except Exception:
                    n = 0
                if n:
                    titles = await page.locator(sel).all_text_contents()
                    sample = " | ".join(t.strip()[:20] for t in titles[:6])
                    print(f"  标题 {sel}: {n} 个 → {sample}", flush=True)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
