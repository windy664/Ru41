"""探针: 盯住「总学习进度」什么时候渲染出来.

背景(2026-11-21 22:33): 第四轮采集后 courses[].progress 仍然全是 None。
怀疑两个原因:
  A) 轮询循环的 break 条件写成「总学习进度 OR 个学习资源」——
     资源数先渲染 → 立刻 break → 进度还没渲染就读不到;
  B) 课程页上根本没有「总学习进度」这几个字(只有资源数)。

本探针对**指定课程 URL** 每 2 秒抓一次文本, 打印两个关键串各自的出现时刻,
直接分辨 A 和 B。

跑法(容器内, 复用 cookies):
    docker run --rm \
      -v "$PWD/config.ini:/app/config.ini:ro" \
      -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" \
      zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_progress_timing.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import re
import sys

sys.path.insert(0, "/scripts")

from collector import close_popups, js_click  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

PROGRESS_RE = re.compile(r"总学习进度\s*(\d{1,3})\s*%")
COUNT_RE = re.compile(r"共\s*(\d+)\s*个学习资源")
# 其它可能承载进度的文案, 一并盯上
ALT_RE = re.compile(r"[^\n]{0,20}进度[^\n]{0,20}")


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(4_000)
            await close_popups(page)
            clicked = await js_click(page, "我的学习")
            print(f"点「我的学习」: {clicked}", flush=True)

            seen_progress = None
            seen_count = None
            for i in range(25):  # 25 × 2s = 50s
                await page.wait_for_timeout(2_000)
                text = await page.evaluate(
                    "document.body ? document.body.innerText.slice(0, 8000) : ''"
                )
                pm = PROGRESS_RE.search(text)
                cm = COUNT_RE.search(text)
                if pm and not seen_progress:
                    seen_progress = (i * 2 + 2, pm.group(1))
                if cm and not seen_count:
                    seen_count = (i * 2 + 2, cm.group(1))
                alts = ALT_RE.findall(text)[:4]
                print(
                    f"  t={i * 2 + 2:>3}s 字数={len(text):>5} "
                    f"进度={'有 ' + pm.group(1) + '%' if pm else '无':<6} "
                    f"资源数={'有 ' + cm.group(1) if cm else '无':<6} "
                    f"进度类文案={alts}",
                    flush=True,
                )
                if seen_progress and seen_count:
                    break
            print(f"\n结论: 进度首次出现={seen_progress} 资源数首次出现={seen_count}")
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
