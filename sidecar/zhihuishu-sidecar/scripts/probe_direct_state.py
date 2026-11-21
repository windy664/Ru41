"""探针: 直接调 detect_course_state, 看它到底返回什么、页面当时是什么文本.

背景(2026-11-21 22:40): 时序探针证明页面 t=2s 就有「总学习进度 0%共 5 个学习资源」,
但采集器写出来的 progress 是 None、content_published 是 True ——
说明函数读到的文本**只有资源数、没有进度**。本探针直调函数并把当时的全文落盘。

跑法(容器内, 复用 cookies):
    docker run --rm \
      -v "$PWD/config.ini:/app/config.ini:ro" \
      -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" \
      zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_direct_state.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import sys

sys.path.insert(0, "/scripts")

from collector import close_popups, detect_course_state  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()


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
            got = await detect_course_state(page)
            print(f"detect_course_state 返回: {got!r}", flush=True)
            text = await page.evaluate(
                "document.body ? document.body.innerText.slice(0, 8000) : ''"
            )
            print(f"调用后页面文本 {len(text)} 字:", flush=True)
            for i, line in enumerate(text.splitlines()):
                if line.strip():
                    print(f"  [{i}] {line[:140]!r}", flush=True)
            with open("/app/data/probe_direct.txt", "w", encoding="utf-8") as fh:
                fh.write(text)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
