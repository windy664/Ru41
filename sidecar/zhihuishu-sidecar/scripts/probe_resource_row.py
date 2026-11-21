"""探针: 学习资源列表里「已完成 / 未完成」的标记长什么样。

背景(2026-11-21): 实测看完一个 4 分 07 秒的 mp4 后, 「总学习进度」从 0% 涨到 20%
(5 个资源, 完成 1 个) → 平台认「看过了」, 自研挂机脚本可行。

挂机要能**跳过已看完的**, 否则每天重复刷同样的视频(浪费+风控)。
本探针把每个资源条目的 outerHTML 打出来, 找完成标记的 class/属性。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_resource_row.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from collector import close_popups, js_click  # noqa: E402
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
            for _ in range(20):
                await page.wait_for_timeout(2_000)
                if await page.evaluate("document.body ? document.body.innerText.length : 0") > 50:
                    break
            await close_popups(page)
            await js_click(page, "我的学习")
            await page.wait_for_timeout(6_000)
            loc = page.get_by_text("学习资源", exact=True)
            if await loc.count():
                try:
                    await loc.first.click(timeout=6_000)
                except Exception:
                    pass
                await page.wait_for_timeout(4_000)

            rows = await page.evaluate(
                """() => {
                    const out = [];
                    const items = document.querySelectorAll('.el-tree-node__content');
                    for (const el of items) {
                        const nameEl = el.querySelector('.file-name');
                        const name = nameEl ? nameEl.textContent : '';
                        const actions = el.querySelector('.node-actions');
                        const icon = el.querySelector('img.file-icon');
                        out.push({
                            name: name.trim().slice(0, 40),
                            icon: icon ? icon.getAttribute('src') : null,
                            actions: actions ? actions.innerHTML.slice(0, 700) : null,
                        });
                    }
                    return out;
                }"""
            )
            print(f"资源条目 {len(rows)} 个", flush=True)
            for r in rows:
                print(f"\n=== {r['name']}", flush=True)
                print(f"    icon: {r['icon']}", flush=True)
                print(f"    actions: {r['actions']}", flush=True)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
