"""课堂签到值守: 轮转巡课监听签到/互动类 API 流量, JSONL 追加落盘 (只读).

部署形态: .223 用户级 timer, 工作日白天每 30 分钟拉起一轮(默认 25 分钟会话),
每轮轮转所有 polymas 课程页. 老师发起签到的窗口内, 课程页产生的相关 API 流量
被原样记录(端点+载荷), 作为实现「签到监测 + QQ 推送」的端点情报;
端点确认后本脚本即可退役.
红线: 只监听网络响应, 绝不点击任何签到/答题入口.

用法: DISPLAY=:98 python -u /scripts/signin_watch.py [minutes=25] [out=data/signin_traffic.jsonl]
"""
import asyncio
import hashlib
import json
import os
import sys
import time
from datetime import datetime

sys.path.insert(0, "/app")

from modules import login as login_mod  # noqa: E402
from modules import slider as slider_mod  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies, save_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

STEP_TIMEOUT = 30_000

# 与 collector.py 有意保持一致的滑块依赖(脚本挂载卷, 不重建镜像)
try:
    import cv2  # noqa: F401
    import numpy as np  # noqa: F401

    CV_MODULES = [np, cv2]
except Exception:
    CV_MODULES = None

# 课堂域关键词: 命中则标 suspect(仅标注, 不过滤——过滤会漏掉命名意外的真实签到端点)
KEYWORDS = ("sign", "interact", "attendance", "rollcall", "checkin", "classroom", "activity", "poll")
# 页面文案中的签到弹窗特征(出现即截图存证)
CHECKIN_HINTS = ("去签到", "立即签到", "发起签到", "签到中", "签到倒计时", "距签到结束")


def append(out: str, record: dict) -> None:
    """JSONL 追加一行, 即时刷盘(签到窗口短, 宁多勿丢)."""
    with open(out, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(record, ensure_ascii=False) + "\n")


def now_iso() -> str:
    return datetime.now().isoformat(timespec="seconds")


async def ensure_login(session, config, cookie_path: str) -> bool:
    """复制自 collector.py(脚本挂载卷不便共享模块): cookies 有效即过, 失效账密重登."""
    page = session.page
    context = session.context
    try:
        await page.goto(config.login_url, wait_until="domcontentloaded", timeout=60_000)
    except Exception as exc:
        print(f"[login] 打开登录页失败: {exc}", flush=True)
        return False
    try:
        await login_mod.wait_for_login_complete(page, timeout=10_000)
    except Exception:
        pass
    if not login_mod.is_login_page(page.url):
        print("[login] cookies 有效", flush=True)
        return True
    if not (config.username and config.password):
        print("[login] cookies 失效且未配置账密, 需要人工登录", flush=True)
        return False

    print("[login] cookies 失效, 尝试账密重登...", flush=True)
    try:
        username = await page.wait_for_selector(
            login_mod.LOGIN_USERNAME_SELECTOR, state="visible", timeout=STEP_TIMEOUT
        )
        password = await page.wait_for_selector(
            login_mod.LOGIN_PASSWORD_SELECTOR, state="visible", timeout=STEP_TIMEOUT
        )
        await username.fill(config.username)
        await password.fill(config.password)
        await login_mod.accept_login_terms(page)
        submit = await page.wait_for_selector(
            login_mod.LOGIN_SUBMIT_SELECTOR, state="visible", timeout=STEP_TIMEOUT
        )
        await page.wait_for_timeout(500)
        await submit.click()
    except Exception as exc:
        print(f"[login] 自动登录控件操作失败: {exc}", flush=True)
        return False

    slider_task = None
    if CV_MODULES:
        slider_task = asyncio.create_task(slider_mod.slider_verify(page, CV_MODULES))
    try:
        await login_mod.wait_for_login_complete(page, timeout=180_000)
    finally:
        if slider_task and not slider_task.done():
            slider_task.cancel()
            await asyncio.gather(slider_task, return_exceptions=True)
    ok = not login_mod.is_login_page(page.url)
    if ok:
        save_cookies(await context.cookies(), cookie_path)
        print("[login] 重登成功, cookies 已更新", flush=True)
    return ok


async def watch_page(page, title: str, url: str, seconds: float, out: str, shot_dir: str) -> None:
    """挂监听打开一个课程页; 全量记录课堂域 JSON 流量, 探测签到弹窗并截图."""
    append(out, {"ts": now_iso(), "event": "visit", "course": title, "url": url})
    seen: set = set()
    dom_hits: set = set()

    async def on_resp(resp):
        try:
            low = str(resp.url).lower()
            if "polymas.com" not in low and "zhihuishu.com" not in low:
                return
            if "json" not in (resp.headers.get("content-type") or ""):
                return
            body = await resp.text()
            # 同页相同 url+body 的重复轮询只记一次; 时间戳单调递增可看调用节奏
            key = hashlib.md5((str(resp.url) + body).encode()).hexdigest()
            if key in seen:
                return
            seen.add(key)
            append(
                out,
                {
                    "ts": now_iso(),
                    "event": "api",
                    "course": title,
                    "url": str(resp.url),
                    "status": resp.status,
                    "suspect": any(k in low for k in KEYWORDS),
                    "body": body[:4000],
                },
            )
        except Exception:
            pass

    async def dom_check() -> None:
        """探测签到弹窗特征词; 命中记录一次并截图存证(只读, 不点击)."""
        try:
            text = await asyncio.wait_for(
                page.evaluate("document.body ? document.body.innerText.slice(0, 4000) : ''"),
                timeout=10,
            )
        except Exception:
            return
        hit = next((h for h in CHECKIN_HINTS if h in text), None)
        if not hit or hit in dom_hits:
            return
        dom_hits.add(hit)
        snippet = text[max(0, text.index(hit) - 60):text.index(hit) + 120]
        append(
            out,
            {"ts": now_iso(), "event": "signin_dom", "course": title, "hint": hit, "snippet": snippet},
        )
        try:
            safe = "".join(c if c.isalnum() else "_" for c in title)[:20]
            path = f"{shot_dir}/{datetime.now().strftime('%Y%m%d_%H%M%S')}_{safe}.png"
            await page.screenshot(path=path, timeout=15_000)
        except Exception:
            pass

    page.on("response", on_resp)
    try:
        await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            await dom_check()
            await page.wait_for_timeout(15_000)
    except Exception as exc:
        append(out, {"ts": now_iso(), "event": "error", "course": title, "error": str(exc)[:200]})
    finally:
        page.remove_listener("response", on_resp)


async def main() -> None:
    minutes = float(sys.argv[1]) if len(sys.argv) > 1 else 25.0
    out = sys.argv[2] if len(sys.argv) > 2 else "data/signin_traffic.jsonl"
    config = Config(config_path="config.ini")
    cookies = load_cookies("data/cookies.json")
    started = time.time()
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            if not await ensure_login(session, config, "data/cookies.json"):
                append(out, {"ts": now_iso(), "event": "login_failed"})
                return
            try:
                with open("data/report.json", encoding="utf-8") as fh:
                    report = json.load(fh)
            except Exception as exc:
                append(out, {"ts": now_iso(), "event": "no_report", "error": str(exc)[:120]})
                return
            courses = [
                (c["title"], c["course_home_url"])
                for c in report.get("courses", [])
                if c.get("platform") == "polymas" and c.get("course_home_url")
            ]
            per_course = max(90.0, min(200.0, minutes * 60 / max(len(courses), 1)))
            shot_dir = "data/signin_shots"
            os.makedirs(shot_dir, exist_ok=True)
            print(f"[watch] {len(courses)} 课程轮转, 每页 {per_course:.0f}s", flush=True)
            for title, url in courses:
                await watch_page(session.page, title, url, per_course, out, shot_dir)
            append(
                out,
                {
                    "ts": now_iso(),
                    "event": "session_end",
                    "courses": len(courses),
                    "seconds": round(time.time() - started),
                },
            )
            print(f"[watch] 完成 (耗时 {time.time() - started:.0f}s)", flush=True)
        finally:
            await session.browser.close()


asyncio.run(main())
