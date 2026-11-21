"""polymas 平台(南京大学 AI教学中心)视频挂机 —— 自研版.

为什么不用上游 Autovisor(2026-11-21 生产机实测):
上游认课**靠 DOM 选择器**(modules/lesson_navigation.py 的 wisdom/fusion/hike/legacy
四类), 我们门户上的 polymas 课**四类全部命中 0** → detect_catalog 抛
「课程目录加载超时」→ 该门课失败 → **整轮 break**(后面的课一节都刷不到)。
而且即使补上目录选择器也还不够: polymas 点资源会**跳页**、播放器在 **iframe** 里,
上游要求「视频与目录同页」(page.wait_for_selector("video")) —— 结构上刷不了。

所以自己写。已实测跑通的流程(2026-11-21):
    课程主页 → 点「我的学习」 → 点「学习资源」标签
    → 列出资源(.el-tree-node__content) → 跳过已完成(success 标记)的
    → 点未完成的 mp4 → 资源详情页 → iframe 里找到 <video> → 播放到结束
    → 回课程页 → 下一个
实测: 看完一个 4 分 07 秒的 mp4, 「总学习进度」0% → 20%(5 个资源完成 1 个)。

风控纪律(重要, recon 文档 §5):
- 账号被封 = 整条省时流水线归零 → **降频保号优先**;
- 每门课有时限(默认取 config.ini 的 limitMaxTime, 30 分钟);
- 每晚最多刷几门(默认 3);
- 倍速上限 1.5(平台播放器最高 1.8), 默认取 config.ini 的 limitSpeed;
- 只在定时器(每晚)跑, 不在白天跑。

用法(容器内, 复用 cookies):
    python3 -u /scripts/brush_polymas.py                    # 按 data/targets.json 跑
    python3 -u /scripts/brush_polymas.py --url <课程URL>     # 直接刷一门(调试用)
    python3 -u /scripts/brush_polymas.py --dry-run          # 只列资源, 不播
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import time

sys.path.insert(0, "/app")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from collector import close_popups, js_click  # noqa: E402
from polymas_state import (  # noqa: E402
    parse_progress,
    parse_resource_rows,
    pick_pending_videos,
)
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

DATA_DIR = "/app/data"
TARGETS_PATH = os.path.join(DATA_DIR, "targets.json")
COOKIES_PATH = os.path.join(DATA_DIR, "cookies.json")

# 每门课刷多久(秒) —— 覆盖 config.ini 的 limitMaxTime
DEFAULT_MAX_MINUTES = 30
# 单个视频最长播多久(秒) —— 防止某个视频卡住整轮
MAX_VIDEO_SECONDS = 45 * 60
# 播放器 iframe 里的 <video> 找不到时的重试次数
VIDEO_RETRY = 3


def log(msg: str) -> None:
    print(f"[brush] {msg}", flush=True)


# ---- 页面操作 ---------------------------------------------------------------

RESOURCE_ROWS_JS = """() => {
    const out = [];
    for (const el of document.querySelectorAll('.el-tree-node__content')) {
        const nameEl = el.querySelector('.file-name');
        if (!nameEl) continue;
        const icon = el.querySelector('img.file-icon');
        const actions = el.querySelector('.node-actions');
        out.push({
            name: (nameEl.textContent || '').trim(),
            icon: icon ? (icon.getAttribute('src') || '') : '',
            actions: actions ? actions.innerHTML : '',
        });
    }
    return out;
}"""


async def goto_resources(page, course_url: str) -> bool:
    """进课程 → 我的学习 → 学习资源。返回是否到位."""
    await page.goto(course_url, wait_until="domcontentloaded", timeout=60_000)
    for _ in range(20):  # 等页面真出内容(慢网/限流时会空)
        await page.wait_for_timeout(2_000)
        if await page.evaluate("document.body ? document.body.innerText.length : 0") > 50:
            break
    await close_popups(page)
    if not await js_click(page, "我的学习"):
        log("点不到「我的学习」")
        return False
    await page.wait_for_timeout(6_000)
    loc = page.get_by_text("学习资源", exact=True)
    if await loc.count():
        try:
            await loc.first.click(timeout=6_000)
        except Exception as exc:
            log(f"点「学习资源」失败: {str(exc)[:80]}")
        await page.wait_for_timeout(4_000)
    return True


async def read_resources(page) -> list[dict]:
    rows = await page.evaluate(RESOURCE_ROWS_JS)
    return parse_resource_rows(rows)


async def read_progress(page) -> tuple[int | None, int | None]:
    text = await page.evaluate(
        "document.body ? document.body.innerText.slice(0, 4000) : ''")
    return parse_progress(text)


async def find_video(page):
    """在页面(含 iframe)里找到 <video>."""
    for frame in page.frames:
        try:
            if await frame.locator("video").count():
                return frame.locator("video").first
        except Exception:
            continue
    return None


async def play_resource(page, name: str, budget: float, speed: float) -> bool:
    """点开一个资源并把它播完。返回是否播完."""
    items = page.locator(".file-name")
    count = await items.count()
    target = None
    for i in range(count):
        txt = (await items.nth(i).text_content() or "").strip()
        if txt == name:
            target = items.nth(i)
            break
    if target is None:
        log(f"  列表里找不到资源 {name!r}")
        return False
    try:
        # 用 locator 点而不是 evaluate 里 el.click(): 点资源会**导航**,
        # 在 evaluate 里点会当场销毁执行上下文。
        await target.click(timeout=15_000)
    except Exception as exc:
        log(f"  点资源失败 {name[:20]}: {str(exc)[:100]}")
        return False
    await page.wait_for_timeout(6_000)

    video = None
    for _ in range(VIDEO_RETRY):
        video = await find_video(page)
        if video is not None:
            break
        await page.wait_for_timeout(4_000)
    if video is None:
        log(f"  {name[:24]}: 找不到 <video>(可能不是可播资源)")
        return False

    try:
        await video.evaluate(
            "(v, s) => { v.muted = true; v.playbackRate = s; v.play(); }", speed
        )
    except Exception as exc:
        log(f"  播放失败 {name[:20]}: {str(exc)[:100]}")
        try:
            await video.click(timeout=8_000)
        except Exception:
            return False

    deadline = time.monotonic() + min(budget, MAX_VIDEO_SECONDS)
    while time.monotonic() < deadline:
        await page.wait_for_timeout(5_000)
        try:
            ended = await video.evaluate("v => v.ended")
            cur = await video.evaluate("v => v.currentTime")
        except Exception:
            break
        if ended:
            log(f"  ✓ {name[:34]} 播完了 ({cur:.0f}s)")
            return True
    log(f"  · {name[:34]} 到时限停({cur:.0f}s)")
    return False


# ---- 一门课 -----------------------------------------------------------------

async def brush_course(page, title: str, course_url: str, max_minutes: int,
                       speed: float, dry_run: bool) -> dict:
    result = {"title": title, "url": course_url, "watched": [], "failed": []}
    if not await goto_resources(page, course_url):
        result["error"] = "进不去学习资源页"
        return result

    before_pct, before_count = await read_progress(page)
    log(f"[{title}] 刷之前: 进度={before_pct}% 资源数={before_count}")

    resources = await read_resources(page)
    for r in resources:
        mark = "已完成" if r["done"] else ("待刷" if r["is_video"] else "非视频")
        log(f"   - {r['name'][:40]:<42} {mark}")

    pending = pick_pending_videos(resources)
    if not pending:
        log(f"[{title}] 没有待刷的视频")
        result["progress_before"] = before_pct
        result["progress_after"] = before_pct
        return result

    if dry_run:
        log(f"[{title}] dry-run: 待刷 {[r['name'][:20] for r in pending]}")
        result["pending"] = [r["name"] for r in pending]
        result["progress_before"] = before_pct
        result["progress_after"] = before_pct
        return result

    budget = max_minutes * 60
    spent = 0.0
    for r in pending:
        left = budget - spent
        if left <= 60:
            log(f"[{title}] 到达时限({max_minutes} 分钟), 停")
            break
        started = time.monotonic()
        ok = await play_resource(page, r["name"], left, speed)
        spent += time.monotonic() - started
        (result["watched"] if ok else result["failed"]).append(r["name"])
        # 回课程页(点资源会跳页, go_back 不稳, 直接重走一遍入口)
        await page.wait_for_timeout(2_000)
        if not await goto_resources(page, course_url):
            log(f"[{title}] 回不到学习资源页, 停")
            break

    after_pct, after_count = await read_progress(page)
    log(f"[{title}] 刷之后: 进度={after_pct}% 资源数={after_count}")
    result["progress_before"] = before_pct
    result["progress_after"] = after_pct
    return result


# ---- 主流程 -----------------------------------------------------------------

def load_targets(path: str) -> list[dict]:
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    except Exception as exc:
        log(f"读不到 {path}: {exc}")
        return []
    out = []
    for c in data.get("courses") or []:
        if not isinstance(c, dict):
            continue
        if c.get("engine") != "polymas":
            continue
        url = (c.get("url") or "").strip()
        if not url:
            continue
        out.append({"title": c.get("title") or url, "url": url})
    return out


async def main_async() -> int:
    parser = argparse.ArgumentParser(description="polymas 视频挂机")
    parser.add_argument("--targets", default=TARGETS_PATH)
    parser.add_argument("--url", action="append", default=[], help="直接刷这门课(可多次)")
    parser.add_argument("--max-courses", type=int, default=3)
    parser.add_argument("--max-minutes", type=int, default=0, help="每门课时限(0=读 config.ini)")
    parser.add_argument("--speed", type=float, default=0.0, help="倍速(0=读 config.ini)")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    config = Config(config_path="config.ini")
    max_minutes = args.max_minutes or int(getattr(config, "limitMaxTime", 0)
                                         or DEFAULT_MAX_MINUTES)
    speed = args.speed or float(getattr(config, "limitSpeed", 1.0) or 1.0)
    speed = max(1.0, min(speed, 1.5))  # 平台最高 1.8, 我们封顶 1.5(风控)
    log(f"每门课时限={max_minutes} 分钟, 倍速={speed}")

    courses = []
    for url in args.url:
        courses.append({"title": url.rsplit("/", 1)[-1] or url, "url": url})
    if not courses:
        courses = load_targets(args.targets)
    courses = courses[: args.max_courses]
    if not courses:
        log("没有要刷的课(targets.json 里没有 engine=polymas 的)")
        return 0
    log(f"今晚刷 {len(courses)} 门: {[c['title'] for c in courses]}")

    cookies = load_cookies(COOKIES_PATH)
    results = []
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            for c in courses:
                log(f"===== {c['title']} =====")
                try:
                    res = await brush_course(
                        page, c["title"], c["url"], max_minutes, speed, args.dry_run
                    )
                except Exception as exc:
                    log(f"{c['title']} 异常: {str(exc)[:150]}")
                    res = {"title": c["title"], "error": str(exc)[:200]}
                results.append(res)
        finally:
            await session.context.browser.close()

    summary = {
        "generated_at": time.strftime("%Y-%m-%d %H:%M:%S"),
        "results": results,
    }
    path = os.path.join(DATA_DIR, "brush_log.json")
    try:
        with open(path, "w", encoding="utf-8") as fh:
            json.dump(summary, fh, ensure_ascii=False, indent=1)
        log(f"日志已写 {path}")
    except Exception as exc:
        log(f"写日志失败: {exc}")

    watched = sum(len(r.get("watched") or []) for r in results)
    log(f"===== 本轮播完 {watched} 个视频 =====")
    return 0


def main() -> int:
    return asyncio.run(main_async())


if __name__ == "__main__":
    sys.exit(main())
