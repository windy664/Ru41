"""wenda 平台作业考试页探针: 加载课程页, 点「作业考试」标签, 记录全部 JSON API (只读).

用法: xvfb 下 python /scripts/probe_wenda_tasks.py <course_url> <out.json>
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


async def main() -> None:
    url, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            traffic: list = []

            async def on_resp(resp):
                try:
                    if "json" not in (resp.headers.get("content-type") or ""):
                        return
                    body = await resp.text()
                    traffic.append({"url": str(resp.url), "status": resp.status, "body": body[:8000]})
                except Exception:
                    pass

            page.on("response", on_resp)
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(6_000)
            # 点「作业考试」标签(只切标签页看列表, 不点任何答题入口)
            clicked = await page.evaluate(
                """() => {
                  const hit = [...document.querySelectorAll('*')]
                    .find(e => e.childElementCount === 0 && e.textContent.trim() === '作业考试');
                  if (hit) { hit.click(); return true; }
                  return false;
                }"""
            )
            print(f"clicked: {clicked}", flush=True)
            await page.wait_for_timeout(7_000)
            text = await page.evaluate(
                "document.body ? document.body.innerText.slice(0, 2500) : ''"
            )
            with open(out, "w", encoding="utf-8") as fh:
                json.dump({"url": url, "clicked": bool(clicked), "text": text,
                           "traffic": traffic}, fh, ensure_ascii=False, indent=1)
            print(f"OK traffic={len(traffic)}", flush=True)
        finally:
            await session.close()


asyncio.run(main())
