"""复用 Autovisor 登录态打开页面, 抓取所有 zhihuishu 域名下的链接.

用法: python /scripts/dump_links.py <url> [out.json]
只做只读浏览, 用于定位"我的课程"入口与课程卡片.
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
    out_path = sys.argv[2] if len(sys.argv) > 2 else None
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(target, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            links = await page.eval_on_selector_all(
                "a",
                "els => els.map(e => ({text: (e.innerText || '').trim().slice(0, 60), href: e.href}))",
            )
            links = [l for l in links if "zhihuishu" in (l["href"] or "") or "polymas" in (l["href"] or "")]
            payload = {"final_url": page.url, "title": await page.title(), "links": links}
            text = json.dumps(payload, ensure_ascii=False, indent=1)
            if out_path:
                with open(out_path, "w", encoding="utf-8") as fh:
                    fh.write(text)
            print(text)
        finally:
            await session.browser.close()


asyncio.run(main())
