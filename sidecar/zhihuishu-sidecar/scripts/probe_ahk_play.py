"""探针: 在课程页上真播一个视频, 抓「学习进度」上报接口。

要回答: 平台是用哪个接口记录「这个微课学到哪了」—— 有了它, 刷课就能照着调。

跑法:
    docker run --rm -e CAS_TGT=<TGT> -e AHK_TASK_ID=<taskId> \
      -v "$PWD/data:/app/data" -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 ... DISPLAY=:99 python -u /scripts/probe_ahk_play.py'
"""
import asyncio
import json
import os
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from playwright.async_api import async_playwright  # noqa: E402

CLASS_ID = int(os.environ.get("AHK_CLASS_ID", "29668"))
COLUMN_ID = int(os.environ.get("AHK_COLUMN_ID", "708120"))
INSTANCE_ID = int(os.environ.get("AHK_INSTANCE_ID", "11187"))

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
COURSE = (
    f"https://gzus.aihaoke.net/student/course/{CLASS_ID}/task"
    f"?instanceId={INSTANCE_ID}&taskType=all-tasks&columnId={COLUMN_ID}&uniqueId=column-{COLUMN_ID}"
)


async def main() -> None:
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
        posts = []

        async def on_req(req):
            if req.method == "GET":
                return
            u = req.url
            if "assets" in u or u.endswith((".png", ".jpg", ".svg", ".css", ".woff2")):
                return
            posts.append((u, (req.post_data or "")[:300]))

        page.on("request", on_req)

        async def on_resp(resp):
            if "updateLearnTask" in resp.url:
                try:
                    body = (await resp.text())[:400]
                except Exception:
                    body = "(读不到)"
                posts.append((resp.url + "  →响应", body))

        page.on("response", on_resp)
        try:
            await page.goto(LOGIN, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(7_000)
            for label in ["去登录", "Go to Login"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    await loc.first.click(timeout=10_000)
                    break
            await page.wait_for_timeout(12_000)
            print(f"登录后: {page.url}", flush=True)

            await page.goto(COURSE, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(12_000)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
            print("\n=== 课程页正文(前 1500 字) ===", flush=True)
            print("\n".join(l.strip() for l in text.splitlines() if l.strip())[:1500], flush=True)

            # 先关掉平台公告弹窗(el-dialog 会挡住点击)
            for sel in [
                ".zt-system-notice-dialog .el-dialog__headerbtn",
                ".el-dialog__headerbtn",
                ".el-overlay-dialog .el-dialog__close",
            ]:
                loc = page.locator(sel)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=5_000, force=True)
                        print(f"关了弹窗({sel})", flush=True)
                        await page.wait_for_timeout(1_500)
                        break
                    except Exception as exc:
                        print(f"关弹窗失败 {sel}: {str(exc)[:60]}", flush=True)
            else:
                await page.keyboard.press("Escape")
                await page.wait_for_timeout(1_000)

            # 点「待学」的微课精讲那一行的 To learn 按钮
            clicked = False
            rows = page.locator("tr").filter(has_text="微课精讲")
            n = await rows.count()
            print(f"\n「微课精讲」行 {n} 个", flush=True)
            for i in range(n):
                row = rows.nth(i)
                rowtext = (await row.inner_text()).replace("\n", " ")
                if "Pending" not in rowtext and "Completed" in rowtext:
                    continue
                btn = row.get_by_text("To learn", exact=False)
                if await btn.count():
                    print(f"点第 {i} 行: {rowtext[:60]}", flush=True)
                    await btn.first.click(timeout=12_000)
                    clicked = True
                    break
            print(f"点了任务: {clicked}", flush=True)
            await page.wait_for_timeout(10_000)
            print(f"\n点后 URL: {page.url}", flush=True)
            vids = await page.locator("video").count()
            print(f"<video> {vids} 个", flush=True)
            if vids:
                v = page.locator("video").first
                try:
                    dur = await v.evaluate("e => e.duration")
                    print(f"时长 {dur}", flush=True)
                    await v.evaluate("e => { e.muted = true; e.play(); }")
                    print("开始播放, 观察 40 秒", flush=True)
                    await page.wait_for_timeout(90_000)
                    ct = await v.evaluate("e => e.currentTime")
                    print(f"currentTime = {ct}", flush=True)
                except Exception as exc:
                    print(f"播放出错: {str(exc)[:120]}", flush=True)

            print("\n=== 播放期间的 POST 请求 ===", flush=True)
            for u, body in posts[-30:]:
                print(f"  {u}\n      {body}", flush=True)
        finally:
            await browser.close()


if __name__ == "__main__":
    asyncio.run(main())
