"""截获「我的任务」页面加载时的 API JSON 响应, 用于任务分类(只读).

用法: xvfb 下 python /scripts/probe_tasks_api.py <course_home_url> <out.json>
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

KEYWORDS = ("task", "exam", "homework", "work", "assignment", "quiz", "test")


async def main() -> None:
    course_url, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    captured: list = []

    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = session.page

        async def on_response(resp):
            try:
                url = resp.url
                if not any(k in url.lower() for k in KEYWORDS):
                    return
                if "json" not in (resp.headers.get("content-type") or ""):
                    return
                body = await resp.text()
                captured.append({"url": url[:200], "status": resp.status, "body": body[:8000]})
            except Exception:
                pass

        page.on("response", on_response)
        try:
            await page.goto(course_url, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            try:
                await page.evaluate(
                    """() => {
                      const hit = [...document.querySelectorAll('*')]
                        .find(e => e.childElementCount === 0 && e.textContent.trim() === '我知道了');
                      if (hit) hit.click();
                    }"""
                )
                await page.wait_for_timeout(1000)
            except Exception:
                pass
            await page.evaluate(
                """() => {
                  const hit = [...document.querySelectorAll('*')]
                    .find(e => e.childElementCount === 0 && e.textContent.trim() === '我的任务');
                  if (hit) hit.click();
                }"""
            )
            await page.wait_for_timeout(10000)
            json.dump(captured, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
            print("OK captured:", len(captured))
        finally:
            await session.browser.close()


asyncio.run(main())
