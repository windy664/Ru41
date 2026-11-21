"""探针: 照抄采集器的新标签页 goto 流程, 看它到底看到了什么文本.

背景(2026-11-21): 点卡片的流程里页面文本是
    「总学习进度 0%共 27 个学习资源」(1050 字, 进度在位置 81, 正则能匹配),
但采集器走的是**新标签页 goto(课程URL)** 的流程, 写出来的 progress 全是 None。
两者只差这个打开方式 —— 所以照抄采集器的流程再抓一次文本。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_goto_state.py; kill $XPID 2>/dev/null'
"""
import asyncio
import sys

sys.path.insert(0, "/scripts")

from collector import (  # noqa: E402
    CARDS_JS,
    detect_course_state,
    CLICK_NTH_CARD_JS,
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
        await wait_for_cards(portal)
        infos = await portal.evaluate(CARDS_JS)

        for index, info in enumerate(infos):
            title = (info or {}).get("title") or "?"
            if "思想道德" not in title and "职业生涯" not in title:
                continue
            # 照抄采集器: 点卡片拿 URL, 再在新标签页 goto
            before = {id(p) for p in portal.context.pages}
            await portal.evaluate(CLICK_NTH_CARD_JS, index)
            url = None
            for _ in range(24):
                await portal.wait_for_timeout(500)
                for p in portal.context.pages:
                    u = p.url or ""
                    if id(p) in before or u.startswith("chrome-error"):
                        continue
                    if "login." not in u and "passport." not in u:
                        url = u
                        break
                if url:
                    break
            if not url:
                print(f"卡 {index} [{title}] 没拿到 URL")
                continue
            print(f"\n=== 卡 {index} [{title}] 课程URL: {url}")
            probe = await portal.context.new_page()
            try:
                await probe.goto(url, wait_until="domcontentloaded", timeout=60_000)
                await probe.wait_for_timeout(4_000)
                await close_popups(probe)
                clicked = await js_click(probe, "我的学习")
                print(f"  点「我的学习」: {clicked}")
                if not clicked:
                    continue
                # 直接调采集器自己的函数, 看它到底返回什么
                got = await detect_course_state(probe)
                print(f"  detect_course_state 返回: {got!r}")
                text = await probe.evaluate("document.body ? document.body.innerText : ''")
                print(f"  文本 {len(text)} 字:")
                for i, line in enumerate(text.splitlines()):
                    if line.strip() and any(
                        k in line for k in ("进度", "资源", "章节", "学习", "%")
                    ):
                        print(f"    [{i}] {line[:120]!r}")
                with open(f"/app/data/probe_goto_{index}.txt", "w", encoding="utf-8") as fh:
                    fh.write(text)
                print(f"  (全文已存 /app/data/probe_goto_{index}.txt)")
                break
            finally:
                await probe.close()
        await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
