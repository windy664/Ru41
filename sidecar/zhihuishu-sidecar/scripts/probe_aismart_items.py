"""探针: ai-smart 课程视频页里「视频」条目的 DOM 与「已学」标记。

要回答两件事:
1. 「视频」条目的 DOM 结构是什么(点它就播放, 但要能枚举出全部条目);
2. 哪些条目**已经学过** —— 已学的必须跳过, 否则每天重刷。
   候选标记: 条目上的 class(learned/done/finished…)、勾选图标、
   旁边的「学习进度 x%」文字、或 <video> 播放后的本地标记。

跑法(容器内, 复用 cookies):
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_aismart_items.py "<课程URL>"; kill $XPID 2>/dev/null'
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

ITEMS = """() => {
    const out = [];
    // 「视频」文字节点, 往上找带结构的容器
    const nodes = Array.from(document.querySelectorAll('*')).filter(
        e => e.children.length === 0 && (e.textContent || '').trim() === '视频');
    for (const el of nodes.slice(0, 4)) {
        let node = el, chain = [];
        for (let i = 0; i < 5 && node; i++) {
            chain.push(node.tagName + '.' + (node.className || '').toString().slice(0, 60));
            node = node.parentElement;
        }
        // 往上找 .row 这一层(整行: 标题 + 视频标签 + 进度/勾)
        let row = el, rowHtml = '';
        for (let i = 0; i < 6 && row; i++) {
            if ((row.className || '').toString().includes('row')) { rowHtml = row.outerHTML.slice(0, 1400); break; }
            row = row.parentElement;
        }
        // .info.course-actions 那一段(视频标签 + 可能的进度/勾) 才是关键
        let info = el, infoHtml = '';
        for (let i = 0; i < 4 && info; i++) {
            if ((info.className || '').toString().includes('course-actions')) { infoHtml = info.outerHTML.slice(0, 900); break; }
            info = info.parentElement;
        }
        // 行内所有 svg 的 class(找「已学」勾的图标)
        let icons = [];
        if (row) icons = Array.from(row.querySelectorAll('svg')).map(s => (s.getAttribute('class') || '') + '|' + (s.querySelector('path') || {}).getAttribute?.('d')?.slice(0, 24));
        out.push({html: infoHtml || rowHtml, chain, icons: icons.slice(0, 8)});
    }
    // 页面上像「进度/已学」的文本
    const prog = Array.from(document.querySelectorAll('*')).filter(
        e => e.children.length === 0 && /已学|已完成|学习进度|掌握度|未学|学习中|\d+%/.test(e.textContent || '')
    ).slice(0, 12).map(e => (e.textContent || '').trim().slice(0, 40));
    return {items: out, prog: prog};
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
            loc = page.get_by_text("课程视频", exact=True)
            if await loc.count():
                await loc.first.click(timeout=8_000)
                await page.wait_for_timeout(7_000)
            print(f"URL: {page.url}", flush=True)
            data = await page.evaluate(ITEMS)
            print(f"\n=== 「视频」条目 {len(data['items'])} 个(取前 3 看结构)", flush=True)
            for it in data["items"][:3]:
                print(f"  祖先链: {' ← '.join(it['chain'])}", flush=True)
                print(f"  course-actions HTML: {it['html'][:600]}", flush=True)
                print(f"  行内 svg: {it.get('icons')}", flush=True)
                print("", flush=True)
            print(f"=== 进度/已学类文本: {data['prog'][:10]}", flush=True)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
