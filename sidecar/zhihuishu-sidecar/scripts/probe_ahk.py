"""探针: 学习平台(gzus.aihaoke.net)的登录方式与课程页结构。

背景(2026-11-22): 用户给了一个课程任务页
    https://gzus.aihaoke.net/student/course/29668/task?instanceId=11187&taskType=all-tasks&columnId=708120
平台是 Vue3 SPA(assets-static.aihaoke.net), 用「教务系统统一身份认证」登录
(和 jwxt 插件同一套 CAS: https://cas.gzus.edu.cn/lyuapServer)。

这个探针要回答:
1. 未登录时它跳到哪(哪个 SSO service 地址)—— 拿它去用 jwxt 已有的 TGT 换 ST;
2. 登录后课程任务页长什么样: 任务清单、视频播放器、进度标记;
3. 播放时往平台上报了什么(学习记录的接口)。

跑法(容器内, 复用 cookies):
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_ahk.py "<课程URL>"; kill $XPID 2>/dev/null'
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


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        ctx = session.context
        page = await ctx.new_page()
        reqs = []

        async def on_req(req):
            u = req.url
            if "assets-static" in u or u.endswith((".png", ".jpg", ".svg", ".woff2", ".css")):
                return
            reqs.append((req.method, u[:160]))

        page.on("request", on_req)
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(12_000)
            print(f"最终 URL: {page.url}", flush=True)
            print(f"标题: {await page.title()}", flush=True)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
            print(f"正文 {len(text)} 字, 前 600:", flush=True)
            print(text[:600], flush=True)
            print("\n=== 网络请求(前 30 条) ===", flush=True)
            for m, u in reqs[:30]:
                print(f"  {m} {u}", flush=True)
            print("\n=== 存储的 cookie 域名 ===", flush=True)
            for c in await ctx.cookies():
                print(f"  {c['domain']} {c['name']}", flush=True)
        finally:
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
