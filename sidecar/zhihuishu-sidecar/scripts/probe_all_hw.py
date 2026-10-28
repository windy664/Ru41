"""批量枚举多门课的作业列表(只读: 只看列表与元数据, 不进入任何答题页).

用法: xvfb 下 python /scripts/probe_all_hw.py <out.json> <课程名1> [课程名2 ...]
"""
import asyncio
import json
import sys

sys.path.insert(0, "/app")

from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()
PORTAL = "https://onlineweb.zhihuishu.com"


async def js_click(page, label: str) -> bool:
    try:
        return await page.evaluate(
            """(label) => {
              const hit = [...document.querySelectorAll('*')]
                .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
              if (hit) { hit.click(); return true; }
              return false;
            }""",
            label,
        )
    except Exception:
        return False


async def page_text(page, n: int = 2500) -> str:
    try:
        return await page.evaluate(
            f"document.body ? document.body.innerText.slice(0, {n}) : ''"
        )
    except Exception:
        return ""


async def probe_course(page, title: str) -> dict:
    # 回到门户(刷新 SPA 状态)
    await page.goto(PORTAL, wait_until="domcontentloaded", timeout=60000)
    await page.wait_for_timeout(9000)
    clicked = await page.evaluate(
        """(label) => {
          const hit = [...document.querySelectorAll('*')]
            .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
          if (hit) { hit.click(); return true; }
          return false;
        }""",
        title,
    )
    if not clicked:
        return {"title": title, "error": "portal 上未找到课程卡片"}
    await page.wait_for_timeout(10000)
    # 课程页: 新 tab 或当前页
    course_page = None
    for p in page.context.pages:
        if p != page and ("course-home" in p.url or "zhihuishu.com/stu" in p.url or "polymas" in p.url):
            course_page = p
    if course_page is None:
        course_page = page
    await course_page.wait_for_timeout(6000)
    result = {
        "title": title,
        "course_home": course_page.url,
        "platform": "polymas" if "polymas" in course_page.url else ("wenda" if "wenda" in course_page.url else "?"),
    }
    result["home_text"] = await page_text(course_page, 1800)
    # 老平台才有 作业考试 tab; 只读列表, 不点任何答题按钮
    if result["platform"] == "wenda":
        await js_click(course_page, "作业考试")
        await course_page.wait_for_timeout(6000)
        result["homework_tab_text"] = await page_text(course_page, 2000)
        result["homework_tab_url"] = course_page.url
    # polymas 平台: 课程主页自带 待办任务/课程通知, home_text 已覆盖
    # 清理多余 tab, 只留门户
    for p in page.context.pages:
        if p != page:
            await p.close()
    return result


async def main() -> None:
    out, titles = sys.argv[1], sys.argv[2:]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            results = []
            for t in titles:
                print("probe:", t, flush=True)
                try:
                    results.append(await probe_course(page, t))
                except Exception as exc:
                    results.append({"title": t, "error": str(exc)[:200]})
            json.dump(results, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
            print("OK", len(results))
        finally:
            await session.browser.close()


asyncio.run(main())
