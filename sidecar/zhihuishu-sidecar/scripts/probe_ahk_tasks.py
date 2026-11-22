"""探针: 登录学习平台 + 用 haoke-token 直接调 API 拿课程任务清单。

登录(2026-11-22 实测):
    课程页 → passport.aihaoke.net/auth/login(学校已自动带出「广州软件学院」)
      → 点「去登录」→ 302 → cas.gzus.edu.cn/lyuapServer/login?service=...
      → CAS 看到 CASTGC(= jwxt 的 TGT)直接放行 → 回跳 → passport 建会话
      → 拿到 gzus.aihaoke.net 的 `haoke-token`(JWT) 与 se-ars1 的 JSESSIONID

⚠️ 生产机 DNS 解析不了 *.gzus.edu.cn → 浏览器加 --host-resolver-rules。

跑法:
    docker run --rm -e CAS_TGT=<TGT> -v "$PWD/data:/app/data" -v "$PWD/scripts:/scripts:ro" \
      zhihuishu-sidecar:local sh -c 'Xvfb :99 ... DISPLAY=:99 python -u /scripts/probe_ahk_tasks.py'
"""
import asyncio
import json
import os
import sys
import urllib.request

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from playwright.async_api import async_playwright  # noqa: E402

CLASS_ID = int(os.environ.get("AHK_CLASS_ID", "29668"))
COLUMN_ID = int(os.environ.get("AHK_COLUMN_ID", "708120"))
INSTANCE_ID = int(os.environ.get("AHK_INSTANCE_ID", "11187"))
API = "https://gzus.aihaoke.net"

HOST_RULES = ",".join(
    [
        "MAP se-ars1.gzus.edu.cn 110.64.203.23",
        "MAP cas.gzus.edu.cn 110.64.203.222",
        "MAP jwxt.gzus.edu.cn 110.64.203.223",
    ]
)

LOGIN = (
    "https://passport.aihaoke.net/auth/login"
    "?tenantSiteUrl=https://gzus.aihaoke.net&locale=en-US"
)


async def login() -> list[dict]:
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
        try:
            await page.goto(LOGIN, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            for label in ["去登录", "Go to Login"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    await loc.first.click(timeout=10_000)
                    print(f"点了「{label}」", flush=True)
                    break
            await page.wait_for_timeout(12_000)
            print(f"登录后 URL: {page.url}", flush=True)
            cookies = await ctx.cookies()
            tok = [c for c in cookies if c["name"] == "haoke-token"]
            print(f"拿到 haoke-token: {bool(tok)}", flush=True)
            return cookies
        finally:
            await browser.close()


def api(cookies: list[dict], path: str, payload: dict) -> dict:
    cookie_hdr = "; ".join(f"{c['name']}={c['value']}" for c in cookies)
    req = urllib.request.Request(
        f"{API}{path}",
        data=json.dumps(payload).encode(),
        headers={
            "Content-Type": "application/json",
            "Cookie": cookie_hdr,
            "User-Agent": "Mozilla/5.0",
            "Referer": "https://gzus.aihaoke.net/",
        },
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read().decode())


async def main() -> None:
    cookies = await login()
    with open("/app/data/ahk_cookies.json", "w", encoding="utf-8") as fh:
        json.dump(cookies, fh, ensure_ascii=False, indent=1)
    payload = {
        "classId": CLASS_ID,
        "columnId": COLUMN_ID,
        "orderType": 0,
        "page": {"pageNo": 1, "pageSize": 50},
        "searchText": "",
        "status": 0,
        "taskTypes": [],
        "requestId": "11111111-2222-3333-4444-555555555555",
    }
    try:
        res = api(cookies, "/api/learn/task/listTask", payload)
        print("\n=== listTask ===", flush=True)
        print(json.dumps(res, ensure_ascii=False)[:4000], flush=True)
    except Exception as exc:
        print(f"listTask 失败: {exc}", flush=True)


if __name__ == "__main__":
    asyncio.run(main())
