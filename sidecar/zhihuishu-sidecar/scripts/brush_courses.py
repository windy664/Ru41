"""刷课入口：按课程 URL / 引擎分发到对应平台的挂机实现。

两个平台的结构完全不同(2026-11-22 生产机实测):
    polymas(南京大学 AI教学中心 hike-teaching-center.polymas.com)
        「我的学习 → 学习资源」→ 点资源 → **跳页** → 资源详情页 → **iframe** 里的 <video>
        资源条目: .file-name;  完成标记: .agent-course-form-item-checkbox success
    aismart(智慧树新版 AI 课 ai-smart-course-student-pro.zhihuishu.com)
        「课程视频」→ 章节列表 → 点「视频」→ **同页**出现 video.js 的 <video>
        条目: .row > .name + .info .tag(视频);  已学标记: 平台不给, 用本地状态文件记

上游 Autovisor 两个平台都刷不了(认课靠 DOM 选择器, 全不命中; 而且它
「一门失败就 break 整轮」), 所以走自研这两条线。

用法(容器内, 复用 cookies):
    python3 -u /scripts/brush_courses.py                 # 按 data/targets.json 刷
    python3 -u /scripts/brush_courses.py --dry-run       # 只列, 不播
    python3 -u /scripts/brush_courses.py --url <课程URL>  # 直接刷某门
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time
from urllib.parse import urlparse

sys.path.insert(0, "/app")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import brush_aismart  # noqa: E402
import brush_polymas  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

DATA_DIR = "/app/data"
TARGETS_PATH = os.path.join(DATA_DIR, "targets.json")
COOKIES_PATH = os.path.join(DATA_DIR, "cookies.json")
DEFAULT_MAX_MINUTES = 30

ENGINE_POLYMAS = "polymas"
ENGINE_AISMART = "aismart"

AISMART_HOST_MARKERS = ("ai-smart-course",)


def log(msg: str) -> None:
    print(f"[brush] {msg}", flush=True)


def engine_for(url: str, declared: str | None = None) -> str | None:
    """这门课该由哪个引擎刷。URL 判定优先(声明的 engine 只作参考)。"""
    host = ""
    try:
        host = urlparse(url).netloc.lower()
    except ValueError:
        host = ""
    if any(marker in host for marker in AISMART_HOST_MARKERS):
        return ENGINE_AISMART
    if declared:
        return declared
    return None


def load_targets(path: str) -> list[dict]:
    """读 targets.json: 取所有带引擎的课程(不是只取 polymas)。"""
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
        url = (c.get("url") or "").strip()
        if not url:
            continue
        engine = engine_for(url, c.get("engine"))
        if not engine:
            continue
        out.append(
            {
                "title": c.get("title") or url.rsplit("/", 1)[-1],
                "url": url,
                "engine": engine,
                "progress": c.get("progress"),
            }
        )
    return out


async def main_async() -> int:
    parser = argparse.ArgumentParser(description="智慧树视频挂机(多平台)")
    parser.add_argument("--targets", default=TARGETS_PATH)
    parser.add_argument("--url", action="append", default=[], help="直接刷这门课(可多次)")
    parser.add_argument("--max-courses", type=int, default=3, help="每个引擎最多刷几门")
    parser.add_argument("--max-minutes", type=int, default=0, help="每门课时限(0=读 config.ini)")
    parser.add_argument("--total-minutes", type=int, default=0,
                        help="整轮时限(0=不限) —— systemd 的 TimeoutStartSec 只有 4 小时, "
                             "不设上限会被杀在半路")
    parser.add_argument("--speed", type=float, default=0.0, help="倍速(0=读 config.ini)")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    config = Config(config_path="config.ini")
    max_minutes = args.max_minutes or int(
        getattr(config, "limitMaxTime", 0) or DEFAULT_MAX_MINUTES
    )
    speed = args.speed or float(getattr(config, "limitSpeed", 1.0) or 1.0)
    speed = max(1.0, min(speed, 1.5))
    log(f"每门课时限={max_minutes} 分钟, 倍速={speed}")

    courses = []
    for url in args.url:
        courses.append(
            {
                "title": url.rstrip("/").rsplit("/", 1)[-1] or url,
                "url": url,
                "engine": engine_for(url),
            }
        )
    if not courses:
        courses = load_targets(args.targets)
    if not courses:
        log("没有要刷的课(targets.json 里没有带引擎的条目)")
        return 0

    per_engine: dict[str, list[dict]] = {}
    for c in courses:
        per_engine.setdefault(c["engine"] or "unknown", []).append(c)
    log(
        "今晚刷: "
        + "; ".join(
            f"{e} → {[x['title'][:16] for x in v[: args.max_courses]]}"
            for e, v in per_engine.items()
        )
    )

    cookies = load_cookies(COOKIES_PATH)
    results = []
    # 整轮预算: 每门课各有时限, 但**整轮**必须有上限 —— systemd 的
    # TimeoutStartSec 只有 4 小时, 5 门 × 90 分钟会被杀在半路(进度只涨一半)。
    deadline = time.monotonic() + args.total_minutes * 60 if args.total_minutes else None
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            for engine, items in per_engine.items():
                for c in items[: args.max_courses]:
                    # 这门课能用多少分钟: 每门上限与整轮剩余取小
                    course_minutes = max_minutes
                    if deadline is not None:
                        left = (deadline - time.monotonic()) / 60
                        if left <= 1:
                            log(f"[整轮预算] 只剩 {left:.0f} 分钟, 收工(剩下的留到明晚)")
                            break
                        course_minutes = int(min(max_minutes, left))
                    log(f"===== {c['title']}（{engine}）=====")
                    try:
                        if engine == ENGINE_AISMART:
                            res = await brush_aismart.brush_course(
                                page, c["title"], c["url"], course_minutes, args.dry_run
                            )
                        elif engine == ENGINE_POLYMAS:
                            res = await brush_polymas.brush_course(
                                page, c["title"], c["url"], course_minutes, speed, args.dry_run
                            )
                        else:
                            res = {"title": c["title"], "error": f"没有 {engine} 引擎"}
                    except Exception as exc:
                        log(f"{c['title']} 异常: {str(exc)[:150]}")
                        res = {"title": c["title"], "error": str(exc)[:200]}
                    results.append(res)
                if deadline is not None and time.monotonic() >= deadline:
                    log("[整轮预算] 用完, 收工(剩下的留到明晚)")
                    break
        finally:
            await session.context.browser.close()

    watched = sum(len(r.get("watched") or []) for r in results)
    log(f"===== 本轮播完 {watched} 个视频 =====")
    try:
        with open(os.path.join(DATA_DIR, "brush_log.json"), "w", encoding="utf-8") as fh:
            json.dump(
                {"generated_at": brush_polymas.time.strftime("%Y-%m-%d %H:%M:%S"),
                 "results": results},
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
