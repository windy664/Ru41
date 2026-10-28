import asyncio, sys
sys.path.insert(0, "/app")
print("P1 imports...", flush=True)
from modules.browser import create_browser_session
from modules.configs import Config
from modules.logger import Logger
from modules.utils import load_cookies
from playwright.async_api import async_playwright
print("P2 building...", flush=True)
logger = Logger()
config = Config(config_path="config.ini")
cookies = load_cookies("data/cookies.json")
print("P3 cookies:", bool(cookies), flush=True)

async def main():
    async with async_playwright() as pw:
        print("P4 pw started", flush=True)
        session = await create_browser_session(pw, config, cookies, logger)
        print("P5 session ok", flush=True)
        page = session.page
        await page.goto("https://passport.zhihuishu.com/login", wait_until="domcontentloaded", timeout=45000)
        print("P6 goto ok:", page.url, flush=True)
        await page.wait_for_timeout(3000)
        print("P7 title:", await page.title(), flush=True)
        await session.browser.close()
        print("P8 closed", flush=True)

asyncio.run(main())
