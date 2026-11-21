"""探针: polymas「我的学习 → 学习资源」的 DOM 与播放行为.

为什么查这个(2026-11-21): Autovisor 首跑在 9 门课上一门都没认出来
(detect_catalog 四类选择器全不命中 → RuntimeError → 整轮 break)。
其中 5 门在 hike-teaching-center.polymas.com(南京大学 AI教学中心),
它们的视频就在「我的学习 → 学习资源」列表里(条目名以 .mp4 结尾)。

上游有 HIKE_CATALOG(.file-item / .icon-finish / .rate)—— polymas 的
"hike-teaching-center" 跟它多半是同一条产品线, 所以「加一个 POLYMAS 目录」
可能是个小补丁; 但得先看清 DOM。

本探针走真实操作路径, 打印:
1. 学习资源列表条目的容器 class / 条目数;
2. 点第一个 .mp4 条目后有没有 <video>、currentTime 是否在走(真的在播)。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_polymas.py "<课程URL>"; kill $XPID 2>/dev/null'
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

DOM_DUMP = """() => {
    const out = {items: [], containers: {}, mp4: []};
    const els = Array.from(document.querySelectorAll('*')).filter(
        e => e.children.length === 0 && /\\.mp4$/i.test((e.textContent || '').trim()));
    for (const el of els.slice(0, 12)) {
        const t = el.textContent.trim().slice(0, 40);
        let node = el, chain = [];
        for (let i = 0; i < 5 && node; i++) {
            chain.push((node.className || node.tagName) + '');
            node = node.parentElement;
        }
        out.mp4.push({text: t, chain});
        for (const c of chain) { out.containers[c] = (out.containers[c] || 0) + 1; }
    }
    out.items = out.mp4;
    out.videos = document.querySelectorAll('video').length;
    out.text = (document.body ? document.body.innerText : '').slice(0, 1500);
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
            clicked = await js_click(page, "我的学习")
            print(f"点「我的学习」: {clicked}", flush=True)
            await page.wait_for_timeout(6_000)
            print(f"当前 URL: {page.url}", flush=True)

            # 「学习资源」标签(默认可能已在)
            for label in ["学习资源"]:
                loc = page.get_by_text(label, exact=True)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=6_000)
                        print(f"点「{label}」", flush=True)
                        await page.wait_for_timeout(4_000)
                    except Exception as exc:
                        print(f"  点 {label} 失败: {str(exc)[:80]}", flush=True)

            info = await page.evaluate(DOM_DUMP)
            print(f"\n页面里 .mp4 结尾的叶子节点: {len(info['items'])} 个; <video>: {info['videos']}",
                  flush=True)
            for it in info["items"][:8]:
                print(f"  {it['text']!r:<44} 链: {' ← '.join(it['chain'][:4])}", flush=True)
            top = sorted(info["containers"].items(), key=lambda kv: -kv[1])[:20]
            print(f"\n容器 class 统计: {top}", flush=True)
            print(f"\n页面文本前 700 字:\n{info['text'][:700]}", flush=True)

            # 点第一个 .mp4 条目, 看会不会起播
            if info["items"]:
                target = page.get_by_text(".mp4", exact=False)
                # 用文本匹配第一个 .mp4 结尾的可见元素
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
                print(f"\n点了 .mp4 条目: {picked!r}", flush=True)
                await page.wait_for_timeout(8_000)
                vids = await page.locator("video").count()
                print(f"点后 <video>: {vids}", flush=True)
                if vids:
                    v = page.locator("video").first
                    for _ in range(3):
                        print(f"  video src={str(await v.get_attribute('src'))[:80]} "
                              f"currentTime={await v.evaluate('e => e.currentTime')} "
                              f"paused={await v.evaluate('e => e.paused')}", flush=True)
                        await page.wait_for_timeout(4_000)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
