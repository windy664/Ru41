"""探针: 学习平台的「统一身份认证(CAS)」登录链路。

要回答: 点 CAS 登录后, 它跳到学校的 CAS 时 **service 参数是什么** ——
那个 service 就是「用 jwxt 已有的 TGT 换 ST」时要用的目标。

跑法(容器内):
    python -u /scripts/probe_ahk_cas.py
"""
import asyncio
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

LOGIN = "https://passport.aihaoke.net/auth/login?tenantSiteUrl=https://gzus.aihaoke.net&locale=en-US"
SCHOOL_HINT = "gzus"


async def dump(page, label: str) -> None:
    text = await page.evaluate("document.body ? document.body.innerText : ''")
    text = "\n".join(line.strip() for line in text.splitlines() if line.strip())
    print(f"\n=== {label} | URL: {page.url}", flush=True)
    print(text[:900], flush=True)


async def main() -> None:
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        ctx = session.context
        page = await ctx.new_page()
        navs = []

        async def on_frame(frame):
            navs.append(frame.url)

        page.on("framenavigated", on_frame)
        try:
            await page.goto(LOGIN, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            await dump(page, "登录页")

            # 切到 CAS 标签(统一身份认证)
            for label in ["统一身份认证", "学校统一身份认证", "CAS", "统一认证"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=6_000)
                        print(f"\n点了「{label}」", flush=True)
                        await page.wait_for_timeout(5_000)
                        break
                    except Exception as exc:
                        print(f"点 {label} 失败: {str(exc)[:80]}", flush=True)
            await dump(page, "CAS 标签页")

            await dump(page, "CAS 标签页(学校已自动带出)")

            # 点「去登录 / Go to Login」→ 跳学校 CAS(service 参数就在那个 URL 里)
            for label in ["去登录", "Go to Login", "前往登录", "进入统一身份认证"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=6_000)
                        print(f"\n点了「{label}」", flush=True)
                        break
                    except Exception as exc:
                        print(f"点 {label} 失败: {str(exc)[:80]}", flush=True)
            await page.wait_for_timeout(10_000)
            await dump(page, "登录之后")

            print("\n=== 导航过的 URL ===", flush=True)
            for u in dict.fromkeys(navs):
                print(f"  {u[:200]}", flush=True)
            print("\n=== cookie 域名 ===", flush=True)
            for c in await ctx.cookies():
                if "aihaoke" in c["domain"] or "gzus" in c["domain"]:
                    print(f"  {c['domain']} {c['name']}", flush=True)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
