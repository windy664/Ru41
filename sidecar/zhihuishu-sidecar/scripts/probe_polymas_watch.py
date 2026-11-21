"""探针: 真的看一遍 polymas 的视频, 看「总学习进度」涨不涨。

这是「自动刷课」可行性的**决定性实验**(2026-11-21):
- 上游 Autovisor 结构上刷不了 polymas(点资源会跳页, 播放器在 iframe 里,
  而上游要求视频和目录同页) → 要自研挂机脚本;
- 但自研值不值得, 取决于**平台认不认「看过了」**。

本探针: 打开一个 mp4 资源 → 在 iframe 里找到 <video> → 播放到结束 →
返回学习资源页 → 读「总学习进度」。进度涨了 → 自研挂机可行。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_polymas_watch.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import re
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

PROGRESS_RE = re.compile(r"总学习进度\s*(\d{1,3})\s*%")


async def read_progress(page) -> str:
    text = await page.evaluate(
        "document.body ? document.body.innerText.slice(0, 4000) : ''")
    m = PROGRESS_RE.search(text)
    count = re.search(r"共\s*(\d+)\s*个学习资源", text)
    return f"进度={m.group(1) if m else '?'}% 资源数={count.group(1) if count else '?'}"


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
            print(f"开始前: {await read_progress(page)}", flush=True)

            # 找到 .mp4 资源条目并点击。
            # ⚠️ 不能在 page.evaluate 里 el.click(): 点资源会**导航**,
            # 执行上下文当场销毁 → evaluate 抛 "Execution context was destroyed"。
            # 用 locator 点, 让 playwright 自己处理导航。
            items = page.locator(".file-name")
            count = await items.count()
            picked = None
            for i in range(count):
                txt = (await items.nth(i).text_content() or "").strip()
                if txt.lower().endswith(".mp4"):
                    picked = txt[:40]
                    await items.nth(i).click(timeout=15_000)
                    break
            print(f"点了资源: {picked!r} (共 {count} 个资源条目)", flush=True)
            await page.wait_for_timeout(7_000)
            print(f"资源页 URL: {page.url}", flush=True)
            await page.screenshot(path="/app/data/watch_1_player.png")

            # 找 iframe 里的 <video>
            video = None
            for frame in page.frames:
                try:
                    if await frame.locator("video").count():
                        video = frame.locator("video").first
                        print(f"在 frame 里找到 video: {frame.url[:90]}", flush=True)
                        break
                except Exception:
                    continue
            if video is None:
                print("没找到 video 元素", flush=True)
                await page.screenshot(path="/app/data/watch_2_novideo.png")
                return

            # 播放
            try:
                await video.evaluate("v => { v.muted = true; v.play(); }")
            except Exception as exc:
                print(f"play() 失败: {str(exc)[:100]}", flush=True)
                try:
                    await video.click(timeout=8_000)
                except Exception as exc2:
                    print(f"click 也失败: {str(exc2)[:100]}", flush=True)

            # 采样播放进度
            for i in range(30):  # 最多 5 分钟
                await page.wait_for_timeout(10_000)
                try:
                    ct = await video.evaluate("v => v.currentTime")
                    dur = await video.evaluate("v => v.duration")
                    ended = await video.evaluate("v => v.ended")
                    paused = await video.evaluate("v => v.paused")
                except Exception as exc:
                    print(f"  采样失败: {str(exc)[:80]}", flush=True)
                    break
                print(f"  t={i*10+10:>3}s currentTime={ct} / {dur} paused={paused} ended={ended}",
                      flush=True)
                if ended:
                    print("  ✅ 视频播完了", flush=True)
                    break
            await page.screenshot(path="/app/data/watch_3_played.png")

            # 回学习资源页, 读进度
            await page.go_back(wait_until="domcontentloaded")
            await page.wait_for_timeout(6_000)
            for _ in range(10):
                await page.wait_for_timeout(2_000)
                if await page.evaluate(
                    "document.body ? document.body.innerText.includes('总学习进度') : false"):
                    break
            print(f"看完后: {await read_progress(page)}", flush=True)
            await page.screenshot(path="/app/data/watch_4_after.png")
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
