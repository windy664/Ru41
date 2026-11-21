"""探针: polymas 点开 .mp4 资源后, 播放器在哪、能不能播。

背景(2026-11-21): polymas「我的学习 → 学习资源」里资源条目 DOM 已经摸清:
    el-tree-node__content > custom-tree-node > node-left > file-name > line1
点第一个 .mp4 条目后**页面上没有 <video>**, 也没崩 —— 播放器可能在 iframe 里、
或者开了新标签页、或者是个自定义播放器组件。

本探针: 点完资源后把 iframe / 新页面 / video 类元素 / media 源全查一遍,
并连续采样 currentTime, 判断到底播没播。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_polymas_play.py "<课程URL>"; kill $XPID 2>/dev/null'
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

AFTER = """() => {
    const out = {};
    out.iframes = Array.from(document.querySelectorAll('iframe')).map(f => f.src || '(no src)');
    out.videos = document.querySelectorAll('video').length;
    out.audios = document.querySelectorAll('audio').length;
    out.sources = Array.from(document.querySelectorAll('source')).map(s => s.src).slice(0, 5);
    out.media = Array.from(document.querySelectorAll('[class*=player],[class*=video],[id*=player],[id*=video],[class*=Player]'))
        .slice(0, 12).map(e => (e.tagName || '') + '.' + (e.className || ''));
    out.text = (document.body ? document.body.innerText : '').slice(0, 600);
    return out;
}"""


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
            print(f"点「我的学习」: {await js_click(page, '我的学习')}", flush=True)
            await page.wait_for_timeout(6_000)
            loc = page.get_by_text("学习资源", exact=True)
            if await loc.count():
                try:
                    await loc.first.click(timeout=6_000)
                except Exception:
                    pass
                await page.wait_for_timeout(4_000)

            before_pages = len(session.context.pages)
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
            print(f"点了: {picked!r}", flush=True)
            await page.wait_for_timeout(8_000)

            print(f"页面数 {before_pages} → {len(session.context.pages)}", flush=True)
            for i, p in enumerate(session.context.pages):
                print(f"  page[{i}] url={p.url[:120]}", flush=True)
                try:
                    info = await p.evaluate(AFTER)
                except Exception as exc:
                    print(f"    取不到: {str(exc)[:80]}", flush=True)
                    continue
                print(f"    iframe={info['iframes']} video={info['videos']} "
                      f"audio={info['audios']}", flush=True)
                print(f"    source={info['sources']}", flush=True)
                print(f"    播放器类元素={info['media']}", flush=True)
                print(f"    文本前 300: {info['text'][:300]!r}", flush=True)
                # 连续采样播放进度
                for _ in range(4):
                    vids = await p.locator("video").count()
                    if not vids:
                        print("    (无 video 元素)", flush=True)
                        break
                    v = p.locator("video").first
                    try:
                        ct = await v.evaluate("e => e.currentTime")
                        paused = await v.evaluate("e => e.paused")
                        dur = await v.evaluate("e => e.duration")
                        print(f"    currentTime={ct} paused={paused} duration={dur}", flush=True)
                    except Exception as exc:
                        print(f"    采样失败: {str(exc)[:80]}", flush=True)
                    await page.wait_for_timeout(4_000)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
