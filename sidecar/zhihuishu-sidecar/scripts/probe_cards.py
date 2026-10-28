"""排查: 门户「我的课程」卡片的 DOM 结构(只读).

用法: xvfb 下 python -u /scripts/probe_cards.py <out.json>
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

SNAP = """() => {
  const known = ['思想道德与法治','国家安全教育','职业生涯规划','形势与政策','军事理论','大学生心理健康'];
  const hits = [];
  for (const el of document.querySelectorAll('*')) {
    if (el.childElementCount > 0) continue;
    const text = (el.textContent || '').trim();
    if (!text || text.length > 30) continue;
    if (!known.some(k => text.includes(k))) continue;
    let node = el;
    for (let up = 0; up < 6 && node.parentElement; up++) {
      node = node.parentElement;
      const cls = (typeof node.className === 'string' ? node.className : '').slice(0, 90);
      const onclick = node.getAttribute('onclick') || '';
      const href = node.getAttribute('href') || '';
      if (cls || onclick || href) {
        hits.push({text, tag: node.tagName, cls, onclick: onclick.slice(0,120), href: href.slice(0,160)});
        break;
      }
    }
  }
  return hits;
}"""


async def main() -> None:
    out = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            await page.goto(PORTAL, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(9000)
            hits = await page.evaluate(SNAP)
            # 顺带 dump 课程区相邻的 API 响应 URL 线索
            body = await page.evaluate(
                "document.body ? document.body.innerText.slice(0, 1500) : ''"
            )
            json.dump({"hits": hits, "body": body}, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
            print("OK hits:", len(hits))
        finally:
            await session.browser.close()


asyncio.run(main())
