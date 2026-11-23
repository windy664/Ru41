"""验证: ai-smart 图谱课 —— 点开资源到底会不会点亮「已完成」。

背景(2026-11-24 凌晨): 「映射」知识点在 kp2 探针时是「必学 1/2」, 在 resource 探针
(点过一次资源) 之后变成「必学 2/2」→ 疑似「点开资源即点亮」。

这个验证做得很干净:
    1. 进 learnPage, 记下所有「必学 n/m」;
    2. 挑一个 **0/N** 的知识点, 点它, 看资源面板里哪些资源「已完成」/未完成;
    3. 点一个**未完成**的资源, 等预览加载完(抓 get-ppt-page / studyRecord 请求);
    4. 回到目录, 看这个知识点是不是变成 1/N —— 变了就说明机制成立。

跑法(容器内):
    python -u /scripts/probe_aismart_complete.py "<课程URL>"
"""
import asyncio
import json
import re
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

# 目录里所有「必学 n/m」+ 它所在条目里的知识点标题
CAT_JS = """() => {
    const out = [];
    // 知识点条目 = .section-item-collapse-info, 文本是「标题 + 必学 n/m」
    for (const el of document.querySelectorAll('.section-item-collapse-info')) {
        const prog = el.querySelector('.progress-text');
        const mark = prog ? (prog.textContent || '').trim() : '';
        const m = mark.match(/必学\\s*(\\d+)\\s*\\/\\s*(\\d+)/);
        if (!m) continue;
        // 标题 = 去掉 progress-text 之后的文本
        const clone = el.cloneNode(true);
        clone.querySelectorAll('.collapse-info-progress').forEach(n => n.remove());
        const title = (clone.textContent || '').trim();
        out.push({
            mark: mark,
            done: parseInt(m[1], 10),
            total: parseInt(m[2], 10),
            title: title.slice(0, 40),
            knowledgeid: el.getAttribute('knowledgeid') || '',
            active: /\\bactive\\b/.test(el.className || ''),
        });
    }
    return out;
}"""

# 资源卡片: 名字 + 是否已完成
RES_JS = """() => {
    const root = document.querySelector('.resources-detail-container');
    if (!root) return [];
    return Array.from(root.querySelectorAll('.basic-info-video-card-container')).map(c => {
        const t = (c.innerText || '').trim().replace(/\\s+/g, ' ');
        return {text: t.slice(0, 90), done: /已完成/.test(t),
                cls: (c.className || '').toString().slice(0, 80)};
    });
}"""


async def main() -> None:
    url = sys.argv[1]
    out_path = "/app/data/probe_aismart_complete.json"
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        hits = []

        async def on_req(request):
            key = request.url.split("/t/")[-1][:90]
            if any(k in key for k in ("studyRecord", "get-ppt-page", "list-knowledge-resource")):
                hits.append({"key": key, "method": request.method})

        page.on("request", on_req)

        async def goto_learn():
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            for _ in range(16):
                await page.wait_for_timeout(1_500)
                if await page.evaluate("document.body ? document.body.innerText.length : 0") > 200:
                    break
            await page.wait_for_timeout(6_000)
            await close_popups(page)
            await page.wait_for_timeout(2_000)
            el = await page.evaluate_handle(
                """() => {
                    const e = document.querySelector('.item-mask') || document.querySelector('.item-content');
                    if (!e) return null;
                    e.setAttribute('data-kp', '1');
                    return e;
                }"""
            )
            node = el.as_element()
            if node:
                try:
                    await node.scroll_into_view_if_needed(timeout=5000)
                    await node.click(timeout=8000, force=True)
                except Exception as exc:
                    print("点卡片失败:", str(exc)[:120])
            await page.wait_for_timeout(10_000)

        await goto_learn()
        print("=== URL ===", page.url)

        cat_before = await page.evaluate(CAT_JS)
        print(f"=== 目录共 {len(cat_before)} 个知识点 ===")
        for c in cat_before[:12]:
            print(f"  {c['mark']}  {c['title']}  kid={c['knowledgeid']}")

        zero = next((c for c in cat_before if re.match(r"必学\s*0\s*/", c["mark"])), None)
        if not zero:
            print("没有 0/N 的知识点了")
            with open(out_path, "w", encoding="utf-8") as fh:
                json.dump({"cat_before": cat_before}, fh, ensure_ascii=False, indent=1)
            return

        print(f"=== 目标: {zero['mark']} {zero['title']} ===")
        clicked_kp = await page.evaluate(
            """(title) => {
                const norm = (s) => (s || '').replace(/\\s+/g, '');
                for (const el of document.querySelectorAll('.section-item-collapse-info')) {
                    const clone = el.cloneNode(true);
                    clone.querySelectorAll('.collapse-info-progress').forEach(n => n.remove());
                    if (norm(clone.textContent) !== norm(title)) continue;
                    // 点条目里的标题(可点击的那个)
                    const t = el.querySelector('[class*=title], [class*=name]')
                          || Array.from(el.querySelectorAll('*')).find(
                              e => !e.children.length && norm(e.textContent) === norm(title));
                    (t || el).click();
                    return true;
                }
                return false;
            }""",
            zero["title"],
        )
        print("点知识点:", clicked_kp)
        await page.wait_for_timeout(9_000)

        res_before = await page.evaluate(RES_JS)
        print("=== 该知识点的资源(点之前) ===")
        print(json.dumps(res_before, ensure_ascii=False, indent=1))

        # 点第一个**未完成**的资源
        hits.clear()
        clicked = await page.evaluate(
            """() => {
                const cands = Array.from(document.querySelectorAll('.basic-info-video-card-container'));
                const e = cands.find(c => !/已完成/.test(c.innerText || ''));
                if (!e) return false;
                e.click();
                return true;
            }"""
        )
        print("已点未完成资源:", clicked)
        if not clicked:
            print("该知识点的资源都已完成(无需验证)")

        await page.wait_for_timeout(25_000)
        print("=== 期间请求 ===")
        for h in hits:
            print(f"  {h['method']} {h['key']}")

        res_after = await page.evaluate(RES_JS)
        print("=== 该知识点的资源(点之后) ===")
        print(json.dumps(res_after, ensure_ascii=False, indent=1))

        # 回目录看计数有没有变 —— 直接重载 learnPage
        await page.reload(wait_until="domcontentloaded", timeout=60_000)
        for _ in range(16):
            await page.wait_for_timeout(1_500)
            if await page.evaluate("document.body ? document.body.innerText.length : 0") > 200:
                break
        await page.wait_for_timeout(7_000)
        await close_popups(page)
        await page.wait_for_timeout(2_000)
        cat_after = await page.evaluate(CAT_JS)
        after = next((c for c in cat_after if c["title"] == zero["title"]), None)
        print("=== 复检 ===")
        print(f"  点之前: {zero['mark']}  {zero['title']}")
        print(f"  点之后: {after['mark'] if after else '?'}  {after['title'] if after else '?'}")

        with open(out_path, "w", encoding="utf-8") as fh:
            json.dump({"cat_before": cat_before, "zero": zero, "res_before": res_before,
                       "res_after": res_after, "hits": hits, "cat_after": cat_after},
                      fh, ensure_ascii=False, indent=1)
        print("已写", out_path)


asyncio.run(main())
