"""抓取课程页关键 API 的请求头 (排查直连调用所需认证, 只读).

用法: xvfb 下 python /scripts/probe_request_headers.py <course_url> <out.json>
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

# 目标端点片段 → 记录其完整请求(头+体)
TARGETS = ("lesson/live/list", "activityTask/list", "get-open-cr", "unread/count")


async def main() -> None:
    url, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            captured: list = []

            def on_request(req):
                try:
                    if not any(t in req.url for t in TARGETS):
                        return
                    entry = {"url": req.url, "method": req.method,
                             "headers": {k: v for k, v in req.headers.items()
                                         if k.lower() not in ("user-agent", "accept-encoding",
                                                              "accept-language", "content-length")}}
                    try:
                        entry["post_data"] = req.post_data
                    except Exception:
                        pass
                    captured.append(entry)
                except Exception:
                    pass

            page.on("request", on_request)
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(15_000)
            with open(out, "w", encoding="utf-8") as fh:
                json.dump(captured, fh, ensure_ascii=False, indent=1)
            print(f"OK captured={len(captured)}", flush=True)
        finally:
            await session.close()


asyncio.run(main())
