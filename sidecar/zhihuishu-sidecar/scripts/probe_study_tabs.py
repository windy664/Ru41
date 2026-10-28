"""studyTask 页标签探针: 点「已完成」标签, 抓列表 API + 文本, 确认补考完成状态 (只读)."""
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

STUDY_TASK_URL = "https://hike-teaching-center.polymas.com/stu-hike/studyTask"

CLICK_LEAF_JS = """(label) => {
  const hit = [...document.querySelectorAll('*')]
    .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
  if (hit) { hit.click(); return true; }
  return false;
}"""


async def main() -> None:
    out = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            traffic: list = []

            async def on_resp(resp):
                try:
                    low = resp.url.lower()
                    if "json" not in (resp.headers.get("content-type") or ""):
                        return
                    if not any(k in low for k in ("task", "todo", "exam", "finish")):
                        return
                    body = await resp.text()
                    traffic.append({"url": str(resp.url), "status": resp.status,
                                    "body": body[:8000]})
                except Exception:
                    pass

            page.on("response", on_resp)
            await page.goto(STUDY_TASK_URL, wait_until="domcontentloaded", timeout=90_000)
            await page.wait_for_timeout(6_000)
            texts: dict = {}
            for label in ("已完成", "全部任务"):
                clicked = await page.evaluate(CLICK_LEAF_JS, label)
                await page.wait_for_timeout(6_000)
                texts[label] = {
                    "clicked": bool(clicked),
                    "text": await page.evaluate(
                        "document.body ? document.body.innerText.slice(0, 1500) : ''"
                    ),
                }
            with open(out, "w", encoding="utf-8") as fh:
                json.dump({"traffic": traffic, "tabs": texts}, fh,
                          ensure_ascii=False, indent=1)
            print(f"OK traffic={len(traffic)}", flush=True)
        finally:
            await session.close()


asyncio.run(main())
