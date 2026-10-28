"""进入作业查看题目内容(只读, 不作答不提交).

用法: xvfb 下 python /scripts/probe_homework.py <course_page_url> <out.json>
流程: 课程页 -> 作业考试 tab -> 开始答题 -> 抓取所有 frame 的文本
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


async def js_click(page, text: str, timeout_hint: str = "") -> bool:
    try:
        return await page.evaluate(
            """(label) => {
              const hit = [...document.querySelectorAll('*')]
                .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
              if (hit) { hit.click(); return true; }
              return false;
            }""",
            text,
        )
    except Exception as exc:
        print(f"js_click {text} failed: {str(exc)[:100]}{timeout_hint}")
        return False


async def dump_frames(page) -> list:
    frames = []
    for f in page.frames:
        try:
            if f == page.main_frame:
                name = "main"
            else:
                name = f.url
            text = await f.evaluate(
                "document.body ? document.body.innerText.slice(0, 5000) : ''"
            )
            if text.strip():
                frames.append({"frame": name[:120], "text": text})
        except Exception:
            continue
    return frames


async def dump_page(page) -> dict:
    try:
        iframes = await page.eval_on_selector_all("iframe", "els => els.map(e => e.src)")
    except Exception:
        iframes = []
    return {
        "url": page.url,
        "title": await page.title(),
        "iframes": iframes,
        "frames": await dump_frames(page),
    }


async def main() -> None:
    course_url, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(course_url, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            await js_click(page, "作业考试")
            await page.wait_for_timeout(6000)
            list_snapshot = await dump_frames(page)
            await js_click(page, "开始答题")
            await page.wait_for_timeout(10000)
            results = []
            for p in page.context.pages:
                try:
                    results.append(await dump_page(p))
                except Exception as exc:
                    results.append({"error": str(exc)[:150], "url": p.url})
            json.dump(
                {"list_snapshot": list_snapshot, "answer_pages": results},
                open(out, "w", encoding="utf-8"),
                ensure_ascii=False,
                indent=1,
            )
            print("OK pages:", len(results))
        finally:
            await session.browser.close()


asyncio.run(main())
