"""一次性 dump: ai-smart 图谱课 learnPage 左侧目录的条目 DOM 结构。

背景(2026-11-24 凌晨): 要写刷课脚本, 但目录里「知识点标题」和「必学 n/m」的
配对方式反复失败(虚拟滚动 + 元素嵌套)。这个探针只做一件事:
把**一个**知识点的完整 DOM 链 dump 出来, 看清结构就够写选择器了。

跑法(容器内):
    python -u /scripts/dump_aismart_catalog.py "<课程URL>"
"""
import asyncio
import json
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from collector import close_popups  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

DUMP_JS = """() => {
    const root = document.querySelector('.section-item-content');
    if (!root) return {error: 'no .section-item-content'};
    const out = {rootClass: root.className, rootHtmlHead: root.outerHTML.slice(0, 1500)};
    // 所有含「必学」的元素, 逐个给出「最内层匹配」+ 它的祖先链
    const all = Array.from(root.querySelectorAll('*'));
    const marks = all.filter(e => /^必学\\s*\\d+\\s*\\/\\s*\\d+$/.test((e.textContent||'').trim()));
    const inner = marks.filter(e => !marks.some(o => o !== e && e.contains(o)));
    out.markCount = marks.length;
    out.innerCount = inner.length;
    out.marks = inner.slice(0, 6).map(e => {
        let chain = [], node = e;
        for (let i = 0; i < 6 && node; i++) {
            chain.push(node.tagName + '.' + (node.className||'').toString().slice(0, 60)
                       + ' | text=' + (node.textContent||'').trim().replace(/\\s+/g,' ').slice(0, 50));
            node = node.parentElement;
        }
        return {text: (e.textContent||'').trim(), tag: e.tagName,
                cls: (e.className||'').toString().slice(0, 60),
                html: e.outerHTML.slice(0, 400),
                chain: chain};
    });
    // 一个完整条目的 outerHTML: 从「必学」往上找一个合理的祖先
    if (inner.length) {
        let p = inner[0];
        for (let i = 0; i < 6 && p; i++) {
            const t = (p.textContent||'').trim();
            if (t.length > 8 && t.length < 120 && /必学/.test(t)) { out.entryHtml = p.outerHTML.slice(0, 2500); out.entryCls = p.className; break; }
            p = p.parentElement;
        }
    }
    // 目录容器是否虚拟滚动: 看 scrollBox 的 scrollHeight/clientHeight
    const sb = document.querySelector('.scroll-box');
    if (sb) out.scroll = {scrollHeight: sb.scrollHeight, clientHeight: sb.clientHeight, scrollTop: sb.scrollTop};
    // 前 30 个叶子节点的文本(看标题/必学怎么排布)
    out.leaves = Array.from(root.querySelectorAll('*'))
        .filter(e => !e.children.length && (e.textContent||'').trim())
        .slice(0, 30)
        .map(e => ({t: (e.textContent||'').trim().slice(0, 24), cls: (e.className||'').toString().slice(0, 40)}));
    return out;
}"""


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
        for _ in range(16):
            await page.wait_for_timeout(1_500)
            if await page.evaluate("document.body ? document.body.innerText.length : 0") > 200:
                break
        await page.wait_for_timeout(6_000)
        await close_popups(page)
        await page.wait_for_timeout(2_000)
        await page.evaluate(
            """() => {
                const e = document.querySelector('.item-mask') || document.querySelector('.item-content');
                if (e) e.click();
            }"""
        )
        await page.wait_for_timeout(11_000)
        dump = await page.evaluate(DUMP_JS)
        print(json.dumps(dump, ensure_ascii=False, indent=1)[:9000])
        with open("/app/data/dump_aismart_catalog.json", "w", encoding="utf-8") as fh:
            json.dump(dump, fh, ensure_ascii=False, indent=1)
        print("已写 /app/data/dump_aismart_catalog.json")


asyncio.run(main())
