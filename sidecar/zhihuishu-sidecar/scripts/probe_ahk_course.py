"""探针: 用教务统一身份(jwxt 已有的 CAS TGT)登录学习平台, 看课程任务页长什么样。

链路(2026-11-22 实测):
    课程页 → passport.aihaoke.net/auth/login(学校已自动带出)
      → 302 gzus.aihaoke.net/api/auth/unified/toAuth
      → 302 se-ars1.gzus.edu.cn/aihaoke/sso.auth?tn=..&ru=..&ck=..&state=..   ← CAS 的 service
      → 302 cas.gzus.edu.cn/lyuapServer/login?service=<上面那个>
      → 登录成功 → 302 回 service → ru=thirdParty/login → passport 建会话 → 回课程页

免密做法: 把 jwxt 已有的 CAS TGT 当 CASTGC cookie 注入(.gzus.edu.cn),
CAS 认出 TGT 就直接放行, 不用输密码和验证码。

⚠️ 生产机 DNS 解析不了 *.gzus.edu.cn → 浏览器加 --host-resolver-rules 映射 IP。

跑法:
    docker run --rm -e CAS_TGT=<TGT> -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 ... DISPLAY=:99 python -u /scripts/probe_ahk_course.py "<课程URL>"'
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


async def main() -> None:
    url = sys.argv[1]
    tgt = (os.environ.get("CAS_TGT") or "").strip()
    if not tgt:
        print("没有 CAS_TGT")
        return
    async with async_playwright() as pw:
        browser = await pw.chromium.launch(
            headless=False,
            args=["--window-size=1600,900", f"--host-resolver-rules={HOST_RULES}", "--no-sandbox"],
        )
        ctx = await browser.new_context()
        # CAS 的 TGT 放 CASTGC cookie(标准 CAS 做法)
        await ctx.add_cookies(
            [{"name": "CASTGC", "value": tgt, "domain": ".gzus.edu.cn", "path": "/"}]
        )
        page = await ctx.new_page()
        api_hits = []

        async def on_resp(resp):
            u = resp.url
            if "api/" in u and "assets" not in u:
                api_hits.append((resp.status, resp.request.method, u[:180]))

        page.on("response", on_resp)
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            # 若停在统一身份登录页: 点「去登录」→ CAS(带 CASTGC cookie)→ 回跳 passport 建会话
            if "passport.aihaoke.net" in page.url:
                for label in ["去登录", "Go to Login"]:
                    loc = page.get_by_text(label, exact=False)
                    if await loc.count():
                        print(f"点了「{label}」", flush=True)
                        await loc.first.click(timeout=10_000)
                        break
                await page.wait_for_timeout(12_000)
                print(f"点完后的 URL: {page.url}", flush=True)
                # 再走一遍课程页, 这时应该已登录
                await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
                await page.wait_for_timeout(12_000)
            print(f"=== 当前 URL ===\n{page.url}", flush=True)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
            print("\n=== 页面正文 ===", flush=True)
            print("\n".join(l.strip() for l in text.splitlines() if l.strip())[:2500], flush=True)
            print("\n=== API 请求(前 30) ===", flush=True)
            for st, m, u in api_hits[:30]:
                print(f"  {st} {m} {u}", flush=True)
            print("\n=== cookie(aihaoke/gzus) ===", flush=True)
            for c in await ctx.cookies():
                if "aihaoke" in c["domain"] or "gzus" in c["domain"]:
                    print(f"  {c['domain']} {c['name']} = {c['value'][:40]}", flush=True)
        finally:
            await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
