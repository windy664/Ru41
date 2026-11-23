"""探针: 依次扫一门 ai-smart 新形态课(图谱课)的各个 tab, 看哪些真有内容。

背景(2026-11-23): 「高等数学I(理)」主页导航:
    知识学习 / AI互动室 / 知识图谱 / 问题图谱 / 任务·作业·考试 / 课后复习 / 课件资料 / 成绩分析
已知: 无「课程视频」入口; 「课件资料」写着「共 0 个学习资源 / 暂无学习资源」。
这个探针把每个 tab 都点一遍, 记录每个 tab 的正文摘要 + 条目数 + 新出现的接口,
以确定这门课到底有没有可刷的内容。

跑法(容器内):
    python -u /scripts/probe_aismart_tabs.py "<课程URL>" [tab1 tab2 ...]
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

SUMMARY_JS = """() => {
    const body = document.body ? document.body.innerText : '';
    // 只取「导航之后的正文」: 去掉前 260 字(导航/欢迎语区)
    const tail = body.slice(260);
    const lines = tail.split('\\n').map(s => s.trim()).filter(Boolean);
    const interesting = lines.filter(l =>
        /资源|视频|文档|课件|作业|考试|任务|复习|章节|暂无|共\\s*\\d+|\\d+\\s*个|学习进度|掌握度|进度/.test(l));
    const rows = Array.from(document.querySelectorAll('.row, .item-content, [class*=resource-item], [class*=file-item], [class*=task-item]'))
        .map(r => (r.innerText || '').trim().replace(/\\s+/g, ' ').slice(0, 90))
        .filter(Boolean);
    return {
        head: body.slice(0, 260),
        tail_head: lines.slice(0, 40),
        interesting: interesting.slice(0, 30),
        rows: rows.slice(0, 25),
        row_count: rows.length,
        videos: document.querySelectorAll('video').length,
        iframes: Array.from(document.querySelectorAll('iframe')).map(f => f.getAttribute('src')).slice(0, 5),
        progress_text: (body.match(/[^\\n]*学习进度[^\\n]*/g) || []).slice(0, 6),
    };
}"""


async def main() -> None:
    url = sys.argv[1]
    tabs = sys.argv[2:] or ["任务·作业·考试", "课后复习", "知识图谱", "问题图谱", "课件资料", "成绩分析"]
    out_path = "/app/data/probe_aismart_tabs.json"
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        api = []

        async def on_resp(resp):
            u = resp.url
            if "/assets/" in u or ".woff" in u or ".css" in u or ".js" in u or "font" in u:
                return
            if "gateway" in u or "/api/" in u:
                api.append({"url": u, "status": resp.status})

        page.on("response", on_resp)
        await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
        for _ in range(16):
            await page.wait_for_timeout(1_500)
            if await page.evaluate("document.body ? document.body.innerText.length : 0") > 200:
                break
        await page.wait_for_timeout(6_000)
        await close_popups(page)
        await page.wait_for_timeout(2_000)

        results = {}
        for tab in tabs:
            api.clear()
            loc = page.get_by_text(tab, exact=True)
            if not await loc.count():
                results[tab] = {"error": "页面上没有这个 tab"}
                print(f"### {tab}: 没有这个 tab")
                continue
            try:
                await loc.first.click(timeout=10_000)
            except Exception as exc:
                results[tab] = {"error": f"点击失败: {str(exc)[:120]}"}
                print(f"### {tab}: 点击失败")
                continue
            await page.wait_for_timeout(7_000)
            s = await page.evaluate(SUMMARY_JS)
            s["api"] = list(api)
            s["url_after"] = page.url
            results[tab] = s
            print("=" * 90)
            print(f"### {tab}  → {s['url_after']}")
            print("  行数:", s["row_count"], " videos:", s["videos"], " iframes:", s["iframes"])
            print("  摘要:", json.dumps(s["tail_head"][:18], ensure_ascii=False))
            if s["interesting"]:
                print("  关键行:", json.dumps(s["interesting"][:14], ensure_ascii=False))
            if s["rows"]:
                print("  条目:", json.dumps(s["rows"][:8], ensure_ascii=False))
            print("  接口:", json.dumps([a["url"].split("/t/")[-1][:70] for a in s["api"]], ensure_ascii=False))

        with open(out_path, "w", encoding="utf-8") as fh:
            json.dump(results, fh, ensure_ascii=False, indent=1)
        print("已写", out_path)


asyncio.run(main())
