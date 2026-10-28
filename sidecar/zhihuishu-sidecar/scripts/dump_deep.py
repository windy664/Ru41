"""深度侦察: 抓取页面渲染后的 innerText / iframe / 全量链接.

用法: xvfb 环境下 python /scripts/dump_deep.py <url> <out.json>
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


async def main() -> None:
    target = sys.argv[1]
    out_path = sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(target, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(12000)
            links = await page.eval_on_selector_all(
                "a",
                "els => els.map(e => ({text: (e.innerText || e.textContent || '').trim().slice(0, 60), href: e.href}))",
            )
            iframes = await page.eval_on_selector_all(
                "iframe", "els => els.map(e => e.src)"
            )
            body_text = await page.evaluate(
                "document.body ? document.body.innerText.slice(0, 4000) : ''"
            )
            payload = {
                "final_url": page.url,
                "title": await page.title(),
                "iframes": iframes,
                "body_text": body_text,
                "links": links,
            }
            with open(out_path, "w", encoding="utf-8") as fh:
                json.dump(payload, fh, ensure_ascii=False, indent=1)
            print("OK", page.url, "links:", len(links), "iframes:", len(iframes))
        finally:
            await session.browser.close()


asyncio.run(main())
