"""探针: 再点一次已看过的视频, 看平台记不记得进度。

判读: 起点 currentTime 接近 0 = 没记账(白播了); 接近片尾/直接结束 = 平台记了。
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

TARGET = "1.1数据类型"   # 已经播完的那个


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, Logger())
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(6_000)
            await close_popups(page)
            loc = page.get_by_text("课程视频", exact=True)
            if await loc.count():
                await loc.first.click(timeout=10_000)
                await page.wait_for_timeout(8_000)
            clicked = await page.evaluate(
                """(target) => {
                    for (const row of document.querySelectorAll('.row')) {
                        const nameEl = row.querySelector('.name');
                        if (((nameEl && nameEl.textContent) || '').trim() !== target) continue;
                        for (const tag of row.querySelectorAll('.info .tag')) {
                            if ((tag.textContent || '').trim() === '视频') { tag.click(); return true; }
                        }
                    }
                    return false;
                }""",
                TARGET,
            )
            print(f"点了「{TARGET}」: {clicked}", flush=True)
            for i in range(12):
                await page.wait_for_timeout(2_500)
                if await page.locator("video").count():
                    v = page.locator("video").first
                    try:
                        ct = await v.evaluate("e => e.currentTime")
                        dur = await v.evaluate("e => e.duration")
                        ended = await v.evaluate("e => e.ended")
                        print(f"  t={i * 2.5:.1f}s currentTime={ct:.1f} duration={dur:.1f} ended={ended}", flush=True)
                        if ct > 5 or ended:
                            break
                    except Exception as exc:
                        print(f"  读取失败: {str(exc)[:70]}", flush=True)
                        break
        finally:
            await session.context.browser.close()


asyncio.run(main())
