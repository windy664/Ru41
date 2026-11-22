"""探针: ai-smart「课程视频」页里点「视频」之后, 播放器长什么样。

背景(2026-11-22): ai-smart 的课程视频页(courseVideoShare)有章节列表, 每章一个
「视频」条目(《程序设计基础》《高等数学I(理)》两门课都是)。但点开之后播放器在哪
——同页的 <video>? iframe? 新标签页? —— 还没摸清。polymas 那边是「点资源 → 跳
资源详情页 → iframe 里一个 <video>」, 这里可能不一样。

跑法(容器内, 复用 cookies):
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_aismart_play.py "<课程URL>"; kill $XPID 2>/dev/null'
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

logger = Logger()


async def dump(page, label: str) -> None:
    info = await page.evaluate(
        """() => {
            const out = {videos: [], iframes: [], text: ''};
            for (const v of document.querySelectorAll('video')) {
                out.videos.push({
                    src: v.getAttribute('src') || (v.querySelector('source') || {}).src || '',
                    cls: (v.className || '') + '',
                });
            }
            for (const f of document.querySelectorAll('iframe')) {
                out.iframes.push({src: f.getAttribute('src') || '', cls: (f.className || '') + ''});
            }
            out.text = (document.body ? document.body.innerText : '').slice(0, 400);
            return out;
        }"""
    )
    print(f"\n=== {label}", flush=True)
    print(f"  URL: {page.url}", flush=True)
    print(f"  <video> {len(info['videos'])}: {info['videos'][:2]}", flush=True)
    print(f"  <iframe> {len(info['iframes'])}: {info['iframes'][:2]}", flush=True)
    print(f"  正文前 250 字: {info['text'][:250]!r}", flush=True)


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(5_000)
            await close_popups(page)
            # 进课程视频页
            loc = page.get_by_text("课程视频", exact=True)
            if await loc.count():
                await loc.first.click(timeout=8_000)
                await page.wait_for_timeout(6_000)
            await dump(page, "课程视频页(点之前)")

            # 列出所有「视频」条目
            items = page.locator(".course-video-share-container *")
            n = await items.count()
            print(f"\n  容器内节点 {n} 个", flush=True)
            vids = page.get_by_text("视频", exact=True)
            cnt = await vids.count()
            print(f"  「视频」条目 {cnt} 个", flush=True)

            before_pages = len(session.context.pages)
            if cnt:
                await vids.first.click(timeout=10_000)
                await page.wait_for_timeout(8_000)
                print(f"\n  页面数 {before_pages} → {len(session.context.pages)}", flush=True)
                for i, p in enumerate(session.context.pages):
                    print(f"   page[{i}] {p.url[:110]}", flush=True)
                await dump(page, "点第一个「视频」后的当前页")
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
