"""探针: ai-smart 课程页上「总学习进度」这类口径到底在哪个页面。

背景(2026-11-22): 播完 1.1数据类型(580s)之后:
- 知识学习页的知识点「学习进度」没变(2% → 2%) —— 那套指标像是算「做题/掌握度」的,
  不一定跟看视频挂钩;
- 课程视频页也没看到「已学」标记。
但 polymas 那边「总学习进度 X%共 N 个学习资源」是**确实会涨**的(0%→60%)。
ai-smart 也有同款文案(高等数学的课件资料页就写着「总学习进度 0% 共 0 个学习资源」),
所以要找到它在**哪一页**, 才知道刷视频到底被不被平台记。

跑法: python -u /scripts/probe_progress_where.py <课程URL>
"""
import asyncio
import re
import sys

sys.path.insert(0, "/scripts")

from collector import close_popups  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

TABS = ["课程视频", "课件资料", "课后复习", "成绩分析"]

PROGRESS_RE = re.compile(r"(总学习进度\s*\d+%\s*共\s*\d+\s*个学习资源|学习进度\s*\d+%|已学\s*\d+)")


async def scan(page, label: str) -> list[str]:
    text = await page.evaluate("document.body ? document.body.innerText : ''")
    hits = [m.group(0).replace("\n", " ") for m in PROGRESS_RE.finditer(text)]
    uniq = []
    for h in hits:
        if h not in uniq:
            uniq.append(h)
    print(f"  [{label}] {page.url.split('/singleCourse/')[-1][:40]} → {uniq[:6] if uniq else '(无进度文案)'}", flush=True)
    return uniq


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
            await scan(page, "知识学习")
            for tab in TABS:
                loc = page.get_by_text(tab, exact=True)
                if not await loc.count():
                    continue
                try:
                    await loc.first.click(timeout=10_000)
                except Exception as exc:
                    print(f"  [{tab}] 点击失败: {str(exc)[:70]}", flush=True)
                    continue
                await page.wait_for_timeout(7_000)
                await scan(page, tab)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
