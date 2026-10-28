"""从课程主页进入"我的学习", 抓取章节/播放页结构.

用法: xvfb 下 python /scripts/probe_study.py <course_home_url> <out.json>
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


async def dump_page(page) -> dict:
    info = {"url": page.url}
    try:
        info["title"] = await page.title()
    except Exception:
        info["title"] = "?"
    try:
        info["iframes"] = await page.eval_on_selector_all(
            "iframe", "els => els.map(e => e.src)"
        )
    except Exception:
        info["iframes"] = []
    try:
        info["videos"] = await page.eval_on_selector_all("video", "els => els.length")
    except Exception:
        info["videos"] = -1
    try:
        info["h264_url"] = await page.evaluate(
            """() => {
              const v = document.querySelector('video');
              return v && (v.currentSrc || v.src) ? String(v.currentSrc || v.src).slice(0, 150) : null;
            }"""
        )
    except Exception:
        info["h264_url"] = None
    try:
        info["text"] = await page.evaluate(
            "document.body ? document.body.innerText.slice(0, 2500) : ''"
        )
    except Exception:
        info["text"] = ""
    return info


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
            # 关掉新手提示弹窗(存在才点)
            try:
                await page.evaluate(
                    """() => {
                      const hit = [...document.querySelectorAll('*')]
                        .find(e => e.childElementCount === 0 && e.textContent.trim() === '我知道了');
                      if (hit) hit.click();
                      return !!hit;
                    }"""
                )
                await page.wait_for_timeout(1500)
            except Exception:
                pass
            try:
                await page.evaluate(
                    """() => {
                      const hit = [...document.querySelectorAll('*')]
                        .find(e => e.childElementCount === 0 && e.textContent.trim() === '我的学习');
                      if (hit) hit.click();
                      return !!hit;
                    }"""
                )
            except Exception as exc:
                print("js click 我的学习 failed:", str(exc)[:120])
            await page.wait_for_timeout(12000)
            results = []
            for p in page.context.pages:
                try:
                    results.append(await dump_page(p))
                except Exception as exc:
                    results.append({"error": str(exc), "url": p.url})
            with open(out, "w", encoding="utf-8") as fh:
                json.dump({"from": course_url, "pages": results}, fh, ensure_ascii=False, indent=1)
            print("OK pages:", len(results))
        finally:
            await session.browser.close()


asyncio.run(main())
