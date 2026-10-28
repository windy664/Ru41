"""点击单个任务, 抓取其落地页(确认页或题目页). 只读: 不点开始/提交.

用法: xvfb 下 python /scripts/probe_task_detail.py <course_home_url> <task_title> <out.json>
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

DANGER_WORDS = ("开始考试", "提交", "交卷", "开始答题")


async def dump_page(page) -> dict:
    info = {"url": page.url, "title": "?"}
    try:
        info["title"] = await page.title()
    except Exception:
        pass
    try:
        info["iframes"] = await page.eval_on_selector_all(
            "iframe", "els => els.map(e => e.src)"
        )
    except Exception:
        info["iframes"] = []
    frames = []
    for f in page.frames:
        try:
            text = await f.evaluate(
                "document.body ? document.body.innerText.slice(0, 6000) : ''"
            )
            if text.strip():
                frames.append({"frame": f.url[:150], "text": text})
        except Exception:
            continue
    info["frames"] = frames
    return info


async def main() -> None:
    course_url, task_title, out = sys.argv[1], sys.argv[2], sys.argv[3]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(course_url, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            try:
                await page.evaluate(
                    """() => {
                      const hit = [...document.querySelectorAll('*')]
                        .find(e => e.childElementCount === 0 && e.textContent.trim() === '我知道了');
                      if (hit) hit.click();
                    }"""
                )
                await page.wait_for_timeout(1200)
            except Exception:
                pass
            await page.evaluate(
                """() => {
                  const hit = [...document.querySelectorAll('*')]
                    .find(e => e.childElementCount === 0 && e.textContent.trim() === '我的任务');
                  if (hit) hit.click();
                }"""
            )
            await page.wait_for_timeout(8000)
            clicked = await page.evaluate(
                """(label) => {
                  const hit = [...document.querySelectorAll('*')]
                    .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
                  if (hit) { hit.click(); return true; }
                  return false;
                }""",
                task_title,
            )
            print("clicked task:", clicked, flush=True)
            await page.wait_for_timeout(12000)
            results = []
            for p in page.context.pages:
                try:
                    results.append(await dump_page(p))
                except Exception as exc:
                    results.append({"error": str(exc)[:150], "url": p.url})
            # 红线检查: 详情页出现危险词 = 考试式, 止步; 确认普通且有"开始答题"才进入
            detail = next(
                (p for p in results if "开始答题" in json.dumps(p, ensure_ascii=False)),
                None,
            )
            danger = ("考试时长", "摄像头", "诚信承诺", "开始考试", "监考")
            detail_text = json.dumps(detail or {}, ensure_ascii=False)
            if detail and not any(w in detail_text for w in danger):
                target_page = next(
                    (p for p in page.context.pages if p.url in (detail.get("url") or "")),
                    None,
                )
                if target_page is not None:
                    clicked_start = await target_page.evaluate(
                        """() => {
                          const hit = [...document.querySelectorAll('*')]
                            .find(e => e.childElementCount === 0 && e.textContent.trim() === '开始答题');
                          if (hit) { hit.click(); return true; }
                          return false;
                        }"""
                    )
                    print("clicked 开始答题:", clicked_start, flush=True)
                    await target_page.wait_for_timeout(12000)
                    results = []
                    for p in page.context.pages:
                        try:
                            results.append(await dump_page(p))
                        except Exception as exc:
                            results.append({"error": str(exc)[:150], "url": p.url})
            else:
                print("RED LINE STOP: 考试式或未见开始答题, 不进入", flush=True)
            json.dump(
                {"clicked": clicked, "task": task_title, "pages": results},
                open(out, "w", encoding="utf-8"),
                ensure_ascii=False,
                indent=1,
            )
            print("OK pages:", len(results))
        finally:
            await session.browser.close()


asyncio.run(main())
