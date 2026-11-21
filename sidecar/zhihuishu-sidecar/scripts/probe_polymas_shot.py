"""探针: polymas 课程页截图 + 播放器定位.

背景(2026-11-21): polymas「我的学习 → 学习资源」的资源条目 DOM 已摸清
(el-tree-node__content > custom-tree-node > node-left > file-name),
但点开 .mp4 后页面上没有 <video> —— 播放器形态不明(iframe? 自定义组件? 新标签页?).

DOM 猜不动就**看**: 每一步截图落盘, 拿回本地直接看页面长什么样。
(截图是 playwright 原生能力, 不用额外依赖。)

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_polymas_shot.py "<课程URL>"; kill $XPID 2>/dev/null'
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

STEP = 0


async def shot(page, label: str) -> None:
    global STEP
    STEP += 1
    path = f"/app/data/poly_{STEP}_{label}.png"
    try:
        await page.screenshot(path=path, full_page=False)
        print(f"[截图] {path}  ({label})", flush=True)
    except Exception as exc:
        print(f"[截图失败] {label}: {str(exc)[:80]}", flush=True)


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            # 等页面真出内容(上次就是没等到, 白跑)
            for i in range(20):
                await page.wait_for_timeout(2_000)
                body = await page.evaluate(
                    "document.body ? document.body.innerText.slice(0,200) : ''")
                if body.strip():
                    print(f"页面有内容了(第 {i+1} 次等待): {body[:80]!r}", flush=True)
                    break
            await close_popups(page)
            await shot(page, "course_home")

            ok = await js_click(page, "我的学习")
            print(f"点「我的学习」: {ok} → {page.url}", flush=True)
            await page.wait_for_timeout(7_000)
            await shot(page, "my_study")

            loc = page.get_by_text("学习资源", exact=True)
            if await loc.count():
                try:
                    await loc.first.click(timeout=6_000)
                    print("点「学习资源」", flush=True)
                except Exception as exc:
                    print(f"点学习资源失败: {str(exc)[:80]}", flush=True)
                await page.wait_for_timeout(5_000)
                await shot(page, "resources")

            # 点第一个 .mp4 条目(用文本节点, 跟列表项一致)
            picked = await page.evaluate(
                """() => {
                    const el = Array.from(document.querySelectorAll('*')).find(
                        e => e.children.length === 0 && /\\.mp4$/i.test((e.textContent||'').trim())
                          && e.offsetParent !== null);
                    if (!el) return null;
                    el.click();
                    return (el.textContent || '').trim().slice(0, 40);
                }"""
            )
            print(f"点了 .mp4: {picked!r}", flush=True)
            await page.wait_for_timeout(5_000)
            await shot(page, "after_click")
            await page.wait_for_timeout(8_000)
            await shot(page, "after_click2")

            print(f"页面数: {len(session.context.pages)}", flush=True)
            for i, p in enumerate(session.context.pages):
                print(f"  page[{i}] {p.url[:130]}", flush=True)
                try:
                    print(f"    video={await p.locator('video').count()} "
                          f"iframe={await p.locator('iframe').count()}", flush=True)
                except Exception:
                    pass
                if i > 0:
                    await shot(p, f"newpage{i}")
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
