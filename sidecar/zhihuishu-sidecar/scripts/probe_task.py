import asyncio, json, sys
sys.path.insert(0, "/app")
from modules.browser import create_browser_session
from modules.configs import Config
from modules.logger import Logger
from modules.utils import load_cookies
from playwright.async_api import async_playwright
logger = Logger()

async def main():
    url, out = sys.argv[1], sys.argv[2]
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        s = await create_browser_session(pw, config, cookies, logger)
        try:
            page = s.page
            await page.goto(url, wait_until="domcontentloaded", timeout=60000)
            await page.wait_for_timeout(8000)
            await page.evaluate("""() => {
              const hit = [...document.querySelectorAll('*')]
                .find(e => e.childElementCount === 0 && e.textContent.trim() === '学习任务');
              if (hit) hit.click();
              return !!hit;
            }""")
            await page.wait_for_timeout(8000)
            data = {
                "url": page.url,
                "text": await page.evaluate("document.body ? document.body.innerText.slice(0, 2500) : ''"),
                "iframes": await page.eval_on_selector_all("iframe", "els => els.map(e => e.src)"),
                "videos": await page.eval_on_selector_all("video", "els => els.length"),
            }
            json.dump(data, open(out, "w"), ensure_ascii=False, indent=1)
            print("OK")
        finally:
            await s.browser.close()

asyncio.run(main())
