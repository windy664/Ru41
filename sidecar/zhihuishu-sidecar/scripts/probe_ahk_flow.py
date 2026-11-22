"""探针: 学习平台走统一身份认证(CAS)的完整跳转链。

要点(2026-11-22 实测):
- 未登录访问课程页 → 跳 passport.aihaoke.net/auth/login?tenantSiteUrl=https://gzus.aihaoke.net
- 登录页默认就在「Unified Identity」标签, 学校**已自动带出**(广州软件学院),
  点「Go to Login」就跳学校 CAS。
- **生产机 DNS 解析不了 *.gzus.edu.cn**(路由器上游查不到), 但 DoH 能查 →
  浏览器加 `--host-resolver-rules` 把 IP 映射进去就能开。

本探针输出:
1. 「Go to Login」之后跳到哪个 CAS 地址(service 参数在里面);
2. CAS 页面长什么样(是否已登录/要不要输密码+验证码)。

跑法(容器内):
    python -u /scripts/probe_ahk_flow.py
"""
import asyncio
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from playwright.async_api import async_playwright  # noqa: E402

LOGIN = "https://passport.aihaoke.net/auth/login?tenantSiteUrl=https://gzus.aihaoke.net&locale=en-US"

# 生产机 DNS 查不到学校域名, 用 DoH 查到的 IP 映射进去
HOST_RULES = ",".join(
    [
        "MAP se-ars1.gzus.edu.cn 110.64.203.23",
        "MAP cas.gzus.edu.cn 110.64.203.222",
        "MAP jwxt.gzus.edu.cn 110.64.203.223",
    ]
)


async def main() -> None:
    async with async_playwright() as pw:
        browser = await pw.chromium.launch(
            headless=False,
            args=[
                "--window-size=1600,900",
                f"--host-resolver-rules={HOST_RULES}",
                "--no-sandbox",
            ],
        )
        ctx = await browser.new_context()
        page = await ctx.new_page()
        hits = []

        async def on_resp(resp):
            u = resp.url
            if "aihaoke" in u or "gzus" in u:
                if "/assets/" in u or ".css" in u or ".png" in u:
                    return
                hits.append((resp.status, resp.request.method, u[:220]))

        page.on("response", on_resp)
        try:
            await page.goto(LOGIN, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
            print("=== 登录页 ===", flush=True)
            print("\n".join(l.strip() for l in text.splitlines() if l.strip())[:500], flush=True)

            for label in ["去登录", "Go to Login"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    await loc.first.click(timeout=8_000)
                    print(f"\n点了「{label}」", flush=True)
                    break
            await page.wait_for_timeout(12_000)

            print(f"\n=== 当前 URL ===\n{page.url}", flush=True)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
            print("\n=== 当前页面正文 ===", flush=True)
            print("\n".join(l.strip() for l in text.splitlines() if l.strip())[:800], flush=True)

            print("\n=== 相关网络请求 ===", flush=True)
            for st, m, u in hits[:25]:
                print(f"  {st} {m} {u}", flush=True)
        finally:
            await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
