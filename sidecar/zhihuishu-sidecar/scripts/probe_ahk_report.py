"""探针: 直接调 updateLearnFile 上报进度, 看 listTask 的 progress 会不会变。

这是「能不能不真播、直接刷」的决定性实验:
    POST /api/learn/task/file/updateLearnFile
        {"classId":29668,"taskId":227778,"position":<秒>,"overFlag":1,...}
    → 再 listTask 看该任务的 progress / doneFlag / myStatus

刷课机制(2026-11-22 实测):
  登录: CAS TGT(借 jwxt 的) → passport → haoke-token
  清单: POST /api/learn/task/listTask  {classId, columnId, page, status:0, taskTypes:[]}
  进度: POST /api/learn/task/file/updateLearnFile {classId, taskId, position, overFlag}
  心跳: POST /api/learn/task/updateLearnTask {classId, taskId}

用法: python -u /scripts/probe_ahk_report.py <classId> <columnId> [taskId]
"""
import asyncio
import json
import os
import sys
import urllib.request
import uuid

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from playwright.async_api import async_playwright  # noqa: E402

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
            await page.wait_for_timeout(7_000)
            for label in ["去登录", "Go to Login"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    await loc.first.click(timeout=10_000)
                    break
            await page.wait_for_timeout(12_000)
            return await ctx.cookies()
        finally:
            await browser.close()


def api(cookies: list[dict], path: str, payload: dict) -> dict:
    cookie_hdr = "; ".join(f"{c['name']}={c['value']}" for c in cookies)
    body = dict(payload)
    body.setdefault("requestId", str(uuid.uuid4()))
    req = urllib.request.Request(
        f"{API}{path}",
        data=json.dumps(body).encode(),
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
    class_id = int(sys.argv[1]) if len(sys.argv) > 1 else 29668
    column_id = int(sys.argv[2]) if len(sys.argv) > 2 else 708120
    only_task = int(sys.argv[3]) if len(sys.argv) > 3 else 0

    cookies = await login()
    print(f"登录 ok, haoke-token={'haoke-token' in [c['name'] for c in cookies]}", flush=True)

    def list_tasks() -> list[dict]:
        res = api(
            cookies,
            "/api/learn/task/listTask",
            {
                "classId": class_id,
                "columnId": column_id,
                "orderType": 0,
                "page": {"pageNo": 1, "pageSize": 50},
                "searchText": "",
                "status": 0,
                "taskTypes": [],
            },
        )
        return (res.get("data") or {}).get("rowList") or []

    tasks = list_tasks()
    print(f"\n=== 任务 {len(tasks)} 个 ===", flush=True)
    for t in tasks:
        print(
            f"  {t['taskId']:>7} type={t['taskType']:>3} progress={t['progress']:>4} "
            f"done={t['doneFlag']} myStatus={t['myStatus']}  {t['taskName'][:34]}",
            flush=True,
        )

    targets = [t for t in tasks if t.get("taskType") == 10 and t.get("progress", 0) < 100]
    if only_task:
        targets = [t for t in targets if t["taskId"] == only_task]
    if not targets:
        print("\n没有待学的微课(taskType=10 且 progress<100)")
        return
    target = targets[0]
    print(f"\n=== 拿 {target['taskName']} (taskId={target['taskId']}) 做实验 ===", flush=True)

    # 上报「看到 999 秒且看完」
    res = api(
        cookies,
        "/api/learn/task/file/updateLearnFile",
        {
            "classId": class_id,
            "taskId": target["taskId"],
            "position": 999,
            "overFlag": 1,
        },
    )
    print(f"updateLearnFile 响应: {json.dumps(res, ensure_ascii=False)[:300]}", flush=True)

    after = list_tasks()
    for t in after:
        if t["taskId"] == target["taskId"]:
            print(
                f"上报后: progress={t['progress']} done={t['doneFlag']} "
                f"myStatus={t['myStatus']} status={t['status']} "
                f"learnTimes={t.get('learnTimes')} submitTimes={t.get('submitTimes')}",
                flush=True,
            )


if __name__ == "__main__":
    asyncio.run(main())
