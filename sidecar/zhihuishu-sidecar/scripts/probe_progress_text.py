"""探针: 把「我的学习」页面上的文字原样抓出来(找进度文案).

目的(2026-11-21): 采集器新增 courses[].progress(总学习进度百分比), 但实测
progress 全是 None —— 正则「总学习进度 X%」没匹配上, 而且多数课程连
「我的学习」都点不到(content=None). 先把真实文本抓出来, 再决定怎么写。

跑法(容器内, 复用 cookies):
    docker run --rm \
      -v "$PWD/config.ini:/app/config.ini:ro" \
      -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" \
      zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_progress_text.py; kill $XPID 2>/dev/null'
"""
import asyncio
import re
import sys
from urllib.parse import urlparse

sys.path.insert(0, "/scripts")

from collector import (  # noqa: E402
    CARDS_JS,
    CLICK_NTH_CARD_JS,
    LOGIN_HOSTS,
    PORTAL,
    close_popups,
    ensure_login,
    goto_portal,
    js_click,
    wait_for_cards,
)
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()


async def dump_text(page, label: str) -> None:
    text = await page.evaluate("document.body ? document.body.innerText : ''")
    print(f"\n===== {label}：页面文本 {len(text)} 字 =====", flush=True)
    for i, line in enumerate(text.splitlines()):
        if not line.strip():
            continue
        if re.search(r"进度|资源|章节|共\s*\d|%|时长|学习", line):
            print(f"  [{i}] {line[:160]}", flush=True)


async def main() -> None:
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        portal = session.page
        if not await ensure_login(session, config, "/app/data/cookies.json"):
            print("登录失败")
            return
        await goto_portal(portal)
        await portal.wait_for_timeout(3_000)
        await close_popups(portal)
        cards = await wait_for_cards(portal)
        infos = await portal.evaluate(CARDS_JS)
        print(f"门户课程卡：{len(cards)} 张", flush=True)

        for index, info in enumerate(infos):
            title = (info or {}).get("title") or "?"
            badge = (info or {}).get("badge") or ""
            before = {id(p) for p in portal.context.pages}
            if not await portal.evaluate(CLICK_NTH_CARD_JS, index):
                continue
            target = None
            for _ in range(24):
                await portal.wait_for_timeout(500)
                for p in portal.context.pages:
                    url = p.url or ""
                    if id(p) in before or url.startswith("chrome-error"):
                        continue
                    if not any(h in (urlparse(url).hostname or "") for h in LOGIN_HOSTS):
                        target = p
                        break
                if target is None and portal.url != PORTAL and "onlineweb" not in portal.url:
                    target = portal
                if target is not None:
                    await target.wait_for_timeout(4_000)
                    break
            if target is None:
                print(f"--- 卡 {index} [{title}] 点开没反应", flush=True)
                continue
            await close_popups(target)
            clicked = await js_click(target, "我的学习")
            print(f"\n--- 卡 {index} [{title}] badge={badge} 点「我的学习」: {clicked}", flush=True)
            if not clicked:
                continue
            await target.wait_for_timeout(6_000)
            await dump_text(target, f"卡 {index} [{title}]")
            text = await target.evaluate("document.body ? document.body.innerText : ''")
            with open(f"/app/data/probe_progress_{index}.txt", "w", encoding="utf-8") as fh:
                fh.write(text)
            print(f"  (全文已存 /app/data/probe_progress_{index}.txt)", flush=True)
            break
        await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
