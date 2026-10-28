"""单独跑 scan_global_todo, 打印归一化后的全局待办条目 (调试用)."""
import asyncio
import json
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from collector import ensure_login, scan_global_todo  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()


async def main() -> None:
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            ok = await ensure_login(session, config, "data/cookies.json")
            print(f"login_ok={ok}", flush=True)
            items = await scan_global_todo(session.page)
            print(json.dumps(items, ensure_ascii=False, indent=1), flush=True)
        finally:
            await session.close()


asyncio.run(main())
