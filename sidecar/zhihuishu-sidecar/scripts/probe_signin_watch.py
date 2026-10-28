"""课堂签到探针: 挂在课程页上监听签到/互动类 API 流量 (只读).

用途: 下次老师发起签到时, 提前在机器上跑本脚本, 抓到端点与载荷后即可实现监测推送.
红线: 只监听网络响应, 不点击任何签到/答题入口.

用法: xvfb 下 python /scripts/probe_signin_watch.py <course_url> <out.json> [minutes=20]
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

# 关注的 API 关键词: 互动/签到/课堂域
KEYWORDS = ("sign", "interact", "attendance", "rollcall", "checkin", "classroom", "activity")


async def main() -> None:
    url, out = sys.argv[1], sys.argv[2]
    minutes = float(sys.argv[3]) if len(sys.argv) > 3 else 20.0
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            traffic: list = []

            async def on_resp(resp):
                try:
                    low = str(resp.url).lower()
                    if "polymas.com" not in low:
                        return
                    if not any(k in low for k in KEYWORDS):
                        return
                    if "json" not in (resp.headers.get("content-type") or ""):
                        return
                    body = await resp.text()
                    traffic.append(
                        {"url": str(resp.url), "status": resp.status, "body": body[:8000]}
                    )
                    # 有新流量就落盘一次, 签到窗口短, 宁多勿丢
                    with open(out, "w", encoding="utf-8") as fh:
                        json.dump({"url": url, "minutes": minutes, "traffic": traffic},
                                  fh, ensure_ascii=False, indent=1)
                except Exception:
                    pass

            page.on("response", on_resp)
            await page.goto(url, wait_until="domcontentloaded", timeout=60000)
            print(f"watching {minutes} min ...", flush=True)
            await page.wait_for_timeout(int(minutes * 60_000))
            with open(out, "w", encoding="utf-8") as fh:
                json.dump({"url": url, "minutes": minutes, "traffic": traffic},
                          fh, ensure_ascii=False, indent=1)
            print(f"OK traffic={len(traffic)}", flush=True)
        finally:
            await session.close()


asyncio.run(main())
