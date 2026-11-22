"""学习平台(广州软件学院 在线好课 gzus.aihaoke.net)课程视频挂机 —— 自研版。

平台结构(2026-11-22 生产机实测):
    课程任务页 /student/course/<classId>/task?instanceId=..&columnId=..
      任务表: 每行一个任务, 列含 State(Completed / Pending Submission) 与 Operate(To learn)
      taskType: 10=微课精讲(视频) / 20=? / 30=AI辩论 / 40=作业 / 50=小测

登录(不需要密码和验证码):
    借 jwxt 插件已经有的 CAS TGT —— 把它当 CASTGC cookie 注入(.gzus.edu.cn),
    CAS 认出 TGT 就直接放行, 走 passport 建会话, 拿到 `haoke-token`。
    ⚠️ 生产机 DNS 解析不了 *.gzus.edu.cn(路由器上游查不到) → 浏览器加
       --host-resolver-rules 映射 DoH 查到的 IP。

进度怎么记(决定性实验):
    播放时播放器周期性 POST:
      /api/learn/task/file/updateLearnFile {classId, taskId, position, overFlag}
      /api/learn/task/updateLearnTask     {classId, taskId}(心跳)
    **直接伪造上报没用**: 把 position 拉满、overFlag=1 发过去, 服务器会把 overFlag
    改回 0、progress 仍是 0 —— 平台按**真实播放**记进度。所以只能真播。

红线:
    只刷 taskType=10 的视频课; 小测(50)/作业(40)/辩论(30) 一律不碰
    (要答题、要提交, 那是「替人做作业」, 不是刷课)。

用法(容器内, 由宿主 unit 注入 CAS_TGT):
    python3 -u /scripts/brush_ahk.py [--class-id 29668] [--column-id 708120]
                                    [--speed 1.5] [--max-videos 3] [--dry-run]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
import urllib.parse
import urllib.request
import uuid

sys.path.insert(0, "/app")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from playwright.async_api import async_playwright  # noqa: E402

API = "https://gzus.aihaoke.net"
LOGIN = (
    "https://passport.aihaoke.net/auth/login"
    "?tenantSiteUrl=https://gzus.aihaoke.net&locale=en-US"
)

# 生产机 DNS 解析不了学校域名 → 用 DoH 查到的 IP 映射进浏览器。
HOST_RULES = ",".join(
    [
        "MAP se-ars1.gzus.edu.cn 110.64.203.23",
        "MAP cas.gzus.edu.cn 110.64.203.222",
        "MAP jwxt.gzus.edu.cn 110.64.203.223",
    ]
)

# 只有微课精讲(视频)才刷
TASK_TYPE_VIDEO = 10
DATA_DIR = os.environ.get("AHK_DATA_DIR", "/app/data")


def log(msg: str) -> None:
    print(f"[brush-ahk] {msg}", flush=True)


def api(cookies: list[dict], path: str, payload: dict) -> dict:
    """调平台 API(带会话 cookie)。"""
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


def list_tasks(cookies: list[dict], class_id: int, column_id: int) -> list[dict]:
    res = api(cookies, "/api/learn/task/listTask", task_list_payload(class_id, column_id))
    return (res.get("data") or {}).get("rowList") or []


async def login() -> list[dict]:
    tgt = (os.environ.get("CAS_TGT") or "").strip()
    if not tgt:
        raise RuntimeError("没有 CAS_TGT(宿主 unit 从 jwxt 的 session.json 里取)")
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
        await ctx.add_cookies(
            [{"name": "CASTGC", "value": tgt, "domain": ".gzus.edu.cn", "path": "/"}]
        )
        page = await ctx.new_page()
        try:
            # 重试: passport 的 toAuth 会生成一次性 ticket/ck, 偶尔因上一轮残留
            # 而落到 checkTenant?error=(空) —— 重新点一次「Go to Login」就好。
            last_url = ""
            for attempt in range(3):
                await page.goto(LOGIN, wait_until="domcontentloaded", timeout=60_000)
                await page.wait_for_timeout(7_000)
                for label in ["去登录", "Go to Login"]:
                    loc = page.get_by_text(label, exact=False)
                    if await loc.count():
                        await loc.first.click(timeout=10_000)
                        break
                await page.wait_for_timeout(12_000)
                cookies = await ctx.cookies()
                if any(c["name"] == "haoke-token" for c in cookies):
                    return cookies
                last_url = page.url
                log(f"  登录没成功(第 {attempt + 1} 次), 停在 {last_url[:110]}")
                await page.wait_for_timeout(4_000)
            raise RuntimeError(f"没拿到 haoke-token, 停在 {last_url}")
        finally:
            await browser.close()


def task_list_payload(class_id: int, column_id: int) -> dict:
    """listTask 的请求体(视频页的 searchData 参数就是它)。"""
    return {
        "classId": class_id,
        "columnId": column_id,
        "orderType": 0,
        "page": {"pageNo": 1, "pageSize": 50},
        "searchText": "",
        "status": 0,
        "taskTypes": [],
        "requireFlag": None,
        "exemptFlag": None,
    }


def video_page_url(task: dict, class_id: int, column_id: int, instance_id: int) -> str:
    """视频页 URL —— 必须和 SPA 拼的**完全一致**。

    ⚠️ 少了 instanceId / searchData / uniqueSite 这些参数时, 页面能打开、<video> 也能播,
    但平台**不上报进度**(2026-11-22 实测: 播完 696s, progress 仍是 0)。
    """
    payload = task_list_payload(class_id, column_id)
    search = urllib.parse.quote(json.dumps(payload, ensure_ascii=False, separators=(",", ":")))
    task_id = task["taskId"]
    return (
        f"https://gzus.aihaoke.net/student/course/{class_id}/studyTask/{task_id}/video"
        f"?taskId={task_id}&instanceId={instance_id}&taskType=all-tasks&columnId={column_id}"
        f"&uniqueId=column-{column_id}&searchData={search}&taskTypeNum={TASK_TYPE_VIDEO}"
    )


def course_list_url(class_id: int, column_id: int, instance_id: int) -> str:
    """课程任务列表页(点「To learn」之前的那个页面)。"""
    return (
        f"https://gzus.aihaoke.net/student/course/{class_id}/task"
        f"?instanceId={instance_id}&taskType=all-tasks&columnId={column_id}"
        f"&uniqueId=column-{column_id}"
    )


async def close_notice_dialog(page) -> None:
    """关掉平台公告弹窗(el-dialog 会挡住点击)。"""
    for sel in [
        ".zt-system-notice-dialog .el-dialog__headerbtn",
        ".el-dialog__headerbtn",
    ]:
        loc = page.locator(sel)
        if await loc.count():
            try:
                await loc.first.click(timeout=5_000, force=True)
                await page.wait_for_timeout(1_500)
                return
            except Exception:
                continue


async def play_video(page, task: dict, class_id: int, column_id: int, speed: float,
                     instance_id: int = 11187) -> bool:
    """打开任务视频页, 播到结束。返回是否播完。"""
    task_id = task["taskId"]
    # ⚠️ 必须**从任务列表点进去**(2026-11-22 实测): 直接开播放页 URL, 视频能播完,
    # 但平台**不上报进度**(progress 一直是 0)—— 平台的学习会话是点「To learn」
    # 时才建立的。所以这里: 先开列表页 → 关弹窗 → 点该行的 To learn。
    await page.goto(
        course_list_url(class_id, column_id, instance_id),
        wait_until="domcontentloaded",
        timeout=60_000,
    )
    await page.wait_for_timeout(9_000)
    await close_notice_dialog(page)
    clicked = False
    rows = page.locator("tr").filter(has_text=task["taskName"][:12])
    for i in range(await rows.count()):
        row = rows.nth(i)
        btn = row.get_by_text("To learn", exact=False)
        if not await btn.count():
            continue
        try:
            await btn.first.click(timeout=12_000)
            clicked = True
            break
        except Exception:
            continue
    if not clicked:
        # 兜底: 直接开播放页(可能不上报进度, 但至少能播)
        await page.goto(
            video_page_url(task, class_id, column_id, instance_id),
            wait_until="domcontentloaded",
            timeout=60_000,
        )
    await page.wait_for_timeout(9_000)
    await close_notice_dialog(page)
    if not await page.locator("video").count():
        log(f"  {task['taskName'][:24]}: 页面里没有 <video>")
        return False
    video = page.locator("video").first
    try:
        dur = await video.evaluate("e => e.duration")
        await video.evaluate(
            "(v, s) => { v.muted = true; v.playbackRate = s; v.play(); }", speed
        )
    except Exception as exc:
        log(f"  {task['taskName'][:24]}: 播放失败 {str(exc)[:90]}")
        return False
    log(f"  {task['taskName'][:26]}: 开始播(时长 {dur:.0f}s, {speed}x)")
    deadline = time.monotonic() + max(120.0, float(dur) / max(speed, 0.1) + 180)
    last = 0.0
    rate_fixed = 0
    while time.monotonic() < deadline:
        await page.wait_for_timeout(5_000)
        try:
            ended = await video.evaluate("e => e.ended")
            cur = await video.evaluate("e => e.currentTime")
            # 平台播放器常在 play/换源之后把 playbackRate 重置回 1 ——
            # 2026-11-22 实测: 设一次不管用, 实播就是 1.0x。每轮补设一次。
            rate = await video.evaluate("e => e.playbackRate")
            if abs(rate - speed) > 0.05:
                await video.evaluate("(v, s) => { v.playbackRate = s; }", speed)
                rate_fixed += 1
                if rate_fixed <= 3:
                    log(f"    倍速被重置成 {rate}x, 已补设 {speed}x")
        except Exception:
            break
        if cur - last >= 60:
            last = cur
            log(f"    {cur / 60:.1f}/{dur / 60:.1f} 分钟")
        if ended:
            log(f"  ✓ {task['taskName'][:26]} 播完了({cur:.0f}s)")
            return True
    log(f"  · {task['taskName'][:26]}: 到时限停")
    return False


async def main_async() -> int:
    parser = argparse.ArgumentParser(description="学习平台课程视频挂机")
    parser.add_argument("--class-id", type=int, default=int(os.environ.get("AHK_CLASS_ID", "29668")))
    parser.add_argument("--column-id", type=int, default=int(os.environ.get("AHK_COLUMN_ID", "708120")))
    parser.add_argument("--speed", type=float, default=1.5)
    parser.add_argument("--instance-id", type=int, default=int(os.environ.get("AHK_INSTANCE_ID", "11187")))
    parser.add_argument("--max-videos", type=int, default=3)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    cookies = await login()
    log("登录 ok(用 jwxt 的 CAS TGT, 没碰密码/验证码)")

    tasks = list_tasks(cookies, args.class_id, args.column_id)
    pending = [
        t
        for t in tasks
        if t.get("taskType") == TASK_TYPE_VIDEO and (t.get("progress") or 0) < 100
    ]
    log(f"任务 {len(tasks)} 个, 待学视频 {len(pending)} 个")
    for t in tasks:
        mark = "待刷" if t in pending else ("视频" if t.get("taskType") == 10 else "其它")
        log(
            f"   {t['taskId']:>7} type={t['taskType']:>3} progress={t['progress']:>4} "
            f"{mark:<4} {t['taskName'][:32]}"
        )
    if not pending:
        log("没有待学的视频课, 收工")
        return 0
    if args.dry_run:
        log("dry-run, 不播")
        return 0

    results = []
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
        for c in cookies:
            try:
                await ctx.add_cookies(
                    [{
                        "name": c["name"],
                        "value": c["value"],
                        "domain": c["domain"],
                        "path": c.get("path", "/"),
                    }]
                )
            except Exception:
                pass
        page = await ctx.new_page()
        try:
            for t in pending[: args.max_videos]:
                ok = await play_video(
                    page, t, args.class_id, args.column_id, args.speed, args.instance_id
                )
                results.append({"taskId": t["taskId"], "name": t["taskName"], "ok": ok})
        finally:
            await browser.close()

    # 回查进度(以平台记的为准)
    after = list_tasks(cookies, args.class_id, args.column_id)
    for r in results:
        cur = next((t for t in after if t["taskId"] == r["taskId"]), None)
        if cur:
            r["progress"] = cur.get("progress")
            r["done"] = cur.get("doneFlag")
            log(f"   {r['name'][:26]}: progress={cur.get('progress')} done={cur.get('doneFlag')}")
    try:
        with open(os.path.join(DATA_DIR, "ahk_brush_log.json"), "w", encoding="utf-8") as fh:
            json.dump(
                {"generated_at": time.strftime("%Y-%m-%d %H:%M:%S"), "results": results},
                fh,
                ensure_ascii=False,
                indent=1,
            )
    except Exception as exc:
        log(f"写日志失败: {exc}")
    return 0


def main() -> int:
    return asyncio.run(main_async())


if __name__ == "__main__":
    sys.exit(main())
