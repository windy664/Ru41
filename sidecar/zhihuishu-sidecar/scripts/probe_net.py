"""探针: 播放时播放器往平台发了什么请求(有没有上报学习记录)。

判读: 出现带 study/record/progress/video 等关键字的 POST/上报请求 → 平台记账了。
"""
import asyncio
import sys

sys.path.insert(0, "/scripts")

from collector import close_popups  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

KEY = ("study", "record", "progress", "learn", "track", "heartbeat", "report", "video")


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, Logger())
        page = await session.context.new_page()
        seen = []

        async def on_req(req):
            u = req.url.lower()
            if req.method != "GET":
                seen.append((req.method, req.url[:150]))

        page.on("request", on_req)
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(6000)
            await close_popups(page)
            loc = page.get_by_text("课程视频", exact=True)
            if await loc.count():
                await loc.first.click(timeout=10000)
                await page.wait_for_timeout(8000)
            await page.evaluate(
                """() => {
                    for (const row of document.querySelectorAll('.row')) {
                        const nameEl = row.querySelector('.name');
                        if (!nameEl) continue;
                        for (const tag of row.querySelectorAll('.info .tag')) {
                            if ((tag.textContent || '').trim() === '视频') { tag.click(); return; }
                        }
                    }
                }"""
            )
            await page.wait_for_timeout(90000)
            print(f"播放 90 秒内的非 GET 请求 {len(seen)} 条:")
            for m, u in seen[:14]:
                print(f"   {m} {u}")
        finally:
            await session.context.browser.close()


asyncio.run(main())
