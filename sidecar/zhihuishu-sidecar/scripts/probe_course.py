"""点击指定课程卡片, 抓取跳转后的学习页结构(只读浏览).

用法: xvfb 下 python /scripts/probe_course.py <课程名> <out.json>
"""
import asyncio
import json
import sys

sys.path.insert(0, "/app")

from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()


async def dump_page(page) -> dict:
    info = {"url": page.url}
    try:
        info["title"] = await page.title()
    except Exception:
        info["title"] = "?"
    try:
        info["iframes"] = await page.eval_on_selector_all(
            "iframe", "els => els.map(e => e.src)"
        )
    except Exception:
        info["iframes"] = []
    try:
        info["videos"] = await page.eval_on_selector_all("video", "els => els.length")
    except Exception:
        info["videos"] = -1
    try:
        info["text"] = await page.evaluate(
            "document.body ? document.body.innerText.slice(0, 1200) : ''"
        )
    except Exception:
        info["text"] = ""
    return info


async def main() -> None:
    title, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(
                "https://onlineweb.zhihuishu.com",
                wait_until="domcontentloaded",
                timeout=60000,
            )
            await page.wait_for_timeout(10000)
            await page.get_by_text(title, exact=True).first.click(timeout=15000)
            await page.wait_for_timeout(12000)
            results = []
            for p in page.context.pages:
                try:
                    results.append(await dump_page(p))
                except Exception as exc:  # 已关闭的页面等
                    results.append({"error": str(exc), "url": p.url})
            with open(out, "w", encoding="utf-8") as fh:
                json.dump({"clicked": title, "pages": results}, fh, ensure_ascii=False, indent=1)
            print("OK pages:", len(results))
        finally:
            await session.browser.close()


asyncio.run(main())
