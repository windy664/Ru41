"""探针: 抓学习平台「学习/任务」相关 API 的请求体与响应(决定刷课怎么做)。

背景(2026-11-22): 注入 CAS 的 CASTGC cookie 后, 平台自己的 API 就通了:
    POST /api/auth/queryCurrentUserInfo   200
    POST /api/learn/task/listTask         200
    POST /api/learn/task/queryClassInfo   200
    POST /api/learn/task/listTaskType    200
    POST /api/learn/home/saveLearnStat    200
所以刷课可以直接照着这些 API 做, 不必走 SPA 的登录跳转。

本探针输出: 每个 /api/learn/* 与 /api/auth/* 的 **请求体** 与 **响应体(截断)**。

跑法:
    docker run --rm -e CAS_TGT=<TGT> -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 ... DISPLAY=:99 python -u /scripts/probe_ahk_api.py "<课程URL>"'
"""
import asyncio
import os
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from playwright.async_api import async_playwright  # noqa: E402

HOST_RULES = ",".join(
    [
        "MAP se-ars1.gzus.edu.cn 110.64.203.23",
        "MAP cas.gzus.edu.cn 110.64.203.222",
        "MAP jwxt.gzus.edu.cn 110.64.203.223",
    ]
)

WATCH = ("/api/learn/", "/api/auth/queryCurrentUserInfo", "/api/core/tenant")


async def main() -> None:
    url = sys.argv[1]
    tgt = (os.environ.get("CAS_TGT") or "").strip()
    async with async_playwright() as pw:
        browser = await pw.chromium.launch(
            headless=False,
            args=["--window-size=1600,900", f"--host-resolver-rules={HOST_RULES}", "--no-sandbox"],
        )
        ctx = await browser.new_context()
        await ctx.add_cookies(
            [{"name": "CASTGC", "value": tgt, "domain": ".gzus.edu.cn", "path": "/"}]
        )
        page = await ctx.new_page()
        bodies = []

        async def on_resp(resp):
            u = resp.url
            if not any(w in u for w in WATCH) or "assets" in u:
                return
            req = resp.request
            post = req.post_data or ""
            try:
                body = (await resp.text())[:1200]
            except Exception as exc:
                body = f"(读取失败 {exc})"
            bodies.append((resp.status, req.method, u.split("?")[0], post[:600], body))

        page.on("response", on_resp)
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(14_000)
            print(f"=== 当前 URL ===\n{page.url}\n", flush=True)
            for st, m, u, post, body in bodies:
                print(f"\n───── {st} {m} {u}", flush=True)
                if post:
                    print(f"  请求体: {post}", flush=True)
                print(f"  响应: {body}", flush=True)
        finally:
            await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
