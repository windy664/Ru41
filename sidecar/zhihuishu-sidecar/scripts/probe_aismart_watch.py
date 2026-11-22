"""探针: ai-smart 视频条目的时长 + 播完之后平台怎么标记「已学」。

要回答:
1. 每个「视频」条目多长(顺带找出最短的, 用来做实验);
2. 播完之后, 该条目的 DOM/文本多出了什么(已学标记? 进度? 图标变了?);
3. 知识学习页的知识点「学习进度」会不会跟着涨(这决定课程级进度怎么算)。

跑法(容器内, 复用 cookies):
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_aismart_watch.py "<课程URL>"; kill $XPID 2>/dev/null'
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

ROW_SNAPSHOT = """() => {
    const out = [];
    for (const row of document.querySelectorAll('.row')) {
        const tag = row.querySelector('.info .tag');
        if (!tag) continue;
        out.push({
            name: (row.querySelector('.name') || {}).textContent || '',
            tag: (tag.textContent || '').trim(),
            tagCls: tag.className || '',
            text: (row.innerText || '').replace(/\\n+/g, ' | ').slice(0, 120),
            html: row.outerHTML.slice(0, 900),
        });
    }
    return out;
}"""

POINTS = """() => {
    const txt = (document.body ? document.body.innerText : '');
    const m = txt.match(/学习进度\\s*(\\d+)%/g) || [];
    const pct = m.map(s => parseInt(s.replace(/\\D+/g, ''), 10)).filter(n => !isNaN(n));
    return {total: pct.length, learned: pct.filter(v => v > 0).length, sample: pct.slice(0, 12)};
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
            await page.wait_for_timeout(5_000)
            await close_popups(page)
            pts = await page.evaluate(POINTS)
            print(f"知识学习页知识点: 共 {pts['total']} 个, 已学 {pts['learned']} 个, 样本 {pts['sample']}", flush=True)

            loc = page.get_by_text("课程视频", exact=True)
            if await loc.count():
                await loc.first.click(timeout=8_000)
                await page.wait_for_timeout(7_000)
            rows = await page.evaluate(ROW_SNAPSHOT)
            vids = [r for r in rows if r["tag"] == "视频"]
            print(f"\n视频条目 {len(vids)} 个", flush=True)
            for r in vids[:3]:
                print(f"  {r['name'][:30]:<32} text={r['text'][:70]!r}", flush=True)

            # 逐个点, 只读时长(不播), 找最短的
            durations = []
            for i in range(min(len(vids), 20)):
                tags = page.locator(".info .tag")
                if i >= await tags.count():
                    break
                try:
                    await tags.nth(i).click(timeout=8_000)
                    await page.wait_for_timeout(4_000)
                    v = page.locator("video").first
                    if await v.count():
                        dur = await v.evaluate("e => e.duration")
                        ct = await v.evaluate("e => e.currentTime")
                        durations.append((i, vids[i]["name"][:26], dur, ct))
                except Exception as exc:
                    print(f"  第 {i} 个点失败: {str(exc)[:60]}", flush=True)
            print("\n=== 时长抽样(序号, 章节, duration, 起始 position) ===", flush=True)
            for d in durations:
                print(f"  {d}", flush=True)
            short = [d for d in durations if d[2] and d[2] > 0]
            if short:
                shortest = min(short, key=lambda d: d[2])
                print(f"\n最短的: {shortest}", flush=True)
                # 播它, 看播完后的标记
                tags = page.locator(".info .tag")
                await tags.nth(shortest[0]).click(timeout=8_000)
                await page.wait_for_timeout(3_000)
                v = page.locator("video").first
                await v.evaluate("e => { e.muted = true; e.playbackRate = 1.8; e.play(); }")
                print(f"  开始播 {shortest[2]:.0f} 秒的视频(1.8 倍速)", flush=True)
                for i in range(int(shortest[2] / 1.8) + 40):
                    await page.wait_for_timeout(5_000)
                    ended = await v.evaluate("e => e.ended")
                    ct = await v.evaluate("e => e.currentTime")
                    if i % 4 == 0:
                        print(f"    t={ct:.0f}/{shortest[2]:.0f} ended={ended}", flush=True)
                    if ended:
                        print("    ✅ 播完", flush=True)
                        break
                await page.wait_for_timeout(5_000)
                rows2 = await page.evaluate(ROW_SNAPSHOT)
                for r in rows2:
                    if r["name"].startswith(shortest[1][:8]):
                        print(f"\n=== 播完后该条目的行 ===\n  text={r['text']!r}\n  html={r['html'][:700]}", flush=True)
                        break
                pts2 = await page.evaluate(POINTS)
                print(f"\n知识点进度: 共 {pts2['total']} 个, 已学 {pts2['learned']} 个", flush=True)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
