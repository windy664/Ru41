"""打开页面 -> JS点击指定标签 -> 抓正文 (只读).

用法: xvfb 下 python /scripts/goto_click_dump.py <url> <label> <out.json>
label 传 "-" 表示不点击, 只抓正文.
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
    url, label, out = sys.argv[1], sys.argv[2], sys.argv[3]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(url, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            if label != "-":
                clicked = await page.evaluate(
                    """(lab) => {
                      const hit = [...document.querySelectorAll('*')]
                        .find(e => e.childElementCount === 0 && e.textContent.trim() === lab);
                      if (hit) { hit.click(); return true; }
                      return false;
                    }""",
                    label,
                )
                print("clicked:", clicked, flush=True)
                await page.wait_for_timeout(7000)
            text = await page.evaluate(
                "document.body ? document.body.innerText.slice(0, 3500) : ''"
            )
            json.dump(
                {"url": page.url, "label": label, "text": text},
                open(out, "w", encoding="utf-8"),
                ensure_ascii=False,
                indent=1,
            )
            print("OK")
        finally:
            await session.browser.close()


asyncio.run(main())
