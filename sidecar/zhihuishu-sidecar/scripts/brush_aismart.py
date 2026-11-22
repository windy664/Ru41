"""ai-smart 新版 AI 课(智慧树)视频挂机 —— 自研版。

平台结构(2026-11-22 生产机实测, 见 scripts/probe_aismart_*.py):
    课程主页 /singleCourse/knowledgeStudy/<courseId>/<chapterId>
      ├─ 「知识学习」: 知识点树, 每个知识点带「掌握度 / 学习进度 X%」  ← 课程级进度信号
      └─ 「课程视频」→ /singleCourse/courseVideoShare/<courseId>/<chapterId>?mapUid=…
            └─ 章节列表: 每行 .row > .name(章节名) + .info.course-actions > .tag(视频)
                  点「视频」→ **同页**出现 video.js 播放器(<video class="vjs-tech">)

跟 polymas 的区别:
    polymas: 点资源 → **跳页** → 资源详情页 → **iframe** 里的 <video>
    ai-smart: 点「视频」→ **同页**出现 <video>

「已学」的判定: 平台在视频条目上不给可读标记(实测 DOM 里只有 icon/subject-icon,
没有 已学/完成 字样), 所以:
    - 课程级进度 → 用知识学习页的知识点「学习进度」(有多少知识点学完);
    - 单个视频 → 用**本地状态文件**记住「我们已经播完哪些」(data/aismart_watched.json),
      避免每天重刷同几个视频。

用法(容器内, 复用 cookies):
    python3 -u /scripts/brush_aismart.py --url <课程URL> [--dry-run]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
import time

sys.path.insert(0, "/app")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from collector import close_popups  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

DATA_DIR = "/app/data"
STATE_PATH = os.path.join(DATA_DIR, "aismart_watched.json")
COOKIES_PATH = os.path.join(DATA_DIR, "cookies.json")

# 单个视频最长播多久(秒); 超时按「没播完」处理, 下次接着来。
MAX_VIDEO_SECONDS = 60 * 60
# 倍速: 平台播放器支持, 但别太猛(风控 + 清晰度)。
SPEED = 1.5

VIDEO_ROWS_JS = """() => {
    const out = [];
    for (const row of document.querySelectorAll('.row')) {
        // ⚠️ 一行可能有**多个**标签(如 ['1个知识点', '视频']), 只看第一个会漏掉一大半
        const tags = Array.from(row.querySelectorAll('.info .tag'))
            .map(t => (t.textContent || '').trim());
        if (!tags.includes('视频')) continue;
        const nameEl = row.querySelector('.name');
        out.push({
            name: ((nameEl && nameEl.textContent) || '').trim(),
        });
    }
    return out;
}"""

KNOWLEDGE_PROGRESS_JS = """() => {
    const text = (document.body ? document.body.innerText : '');
    const m = text.match(/学习进度\\s*(\\d+)%/g) || [];
    const pcts = m.map(s => parseInt(s.replace(/\\D+/g, ''), 10)).filter(n => !isNaN(n));
    const learned = pcts.filter(v => v > 0).length;
    return {
        total: pcts.length,
        learned: learned,
        percent: pcts.length ? Math.round(100 * learned / pcts.length) : null,
    };
}"""


def log(msg: str) -> None:
    print(f"[brush-ai] {msg}", flush=True)


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as fh:
            return json.load(fh)
    except Exception:
        return {}


def save_state(state: dict) -> None:
    try:
        with open(STATE_PATH, "w", encoding="utf-8") as fh:
            json.dump(state, fh, ensure_ascii=False, indent=1)
    except Exception as exc:
        log(f"状态写盘失败: {exc}")


async def goto_video_page(page, course_url: str) -> bool:
    """进课程主页 → 点「课程视频」→ 到视频列表页。

    ⚠️ 必须等 SPA **真的加载完**再点(2026-11-22 实测): 只等「正文有内容」就点,
    页面还在渲染 → 点完只出 5 行(真实有 31 行/18 个视频), 而且不会再补齐。
    判据 = 导航里出现「课程视频」+ 正文够长, 再多等几秒让它稳。
    """
    await page.goto(course_url, wait_until="domcontentloaded", timeout=60_000)
    for _ in range(20):
        await page.wait_for_timeout(1_500)
        body_len = await page.evaluate(
            "document.body ? document.body.innerText.length : 0"
        )
        if body_len > 200 and await page.get_by_text("课程视频", exact=True).count():
            break
    await page.wait_for_timeout(3_000)
    await close_popups(page)
    loc = page.get_by_text("课程视频", exact=True)
    if not await loc.count():
        log("页面上没有「课程视频」入口")
        return False
    try:
        await loc.first.click(timeout=10_000)
    except Exception as exc:
        log(f"点「课程视频」失败: {str(exc)[:100]}")
        return False
    # 视频列表是逐步渲染的: 6 秒只有 5 行, 8 秒才出齐 18 个 → 多等
    await page.wait_for_timeout(8_000)
    return True


async def list_videos(page) -> list[dict]:
    """列出课程视频页里的「视频」条目。

    ⚠️ 这个列表**渲染很慢**: 刚进页面只有 5 行, 等到 8 秒才出齐 18 行
    (实测程序设计基础)。而且**不能乱滚** —— 滚动会让它重新渲染, 反而只剩几行。
    所以这里只「等」: 每 2 秒数一次, 连续几轮没有新增就收工。
    """
    seen: dict[str, dict] = {}
    stagnant = 0
    for _ in range(20):  # 最多等 ~40 秒
        rows = await page.evaluate(VIDEO_ROWS_JS)
        grew = False
        for r in rows:
            name = (r.get("name") or "").strip()
            if name and name not in seen:
                seen[name] = {"name": name}
                grew = True
        if grew:
            stagnant = 0
        else:
            stagnant += 1
            if stagnant >= 3:
                break
        await page.wait_for_timeout(2_000)
    return list(seen.values())


async def play_video(page, name: str, budget: float) -> bool:
    """点某个章节的「视频」并播到结束。"""
    rows = await page.evaluate(VIDEO_ROWS_JS)
    target = None
    for r in rows:
        if (r.get("name") or "").strip() == name:
            target = r
            break
    if target is None:
        log(f"  列表里找不到视频「{name[:24]}」")
        return False
    try:
        await page.evaluate(
            """(name) => {
                for (const row of document.querySelectorAll('.row')) {
                    const nameEl = row.querySelector('.name');
                    if (((nameEl && nameEl.textContent) || '').trim() !== name) continue;
                    for (const tag of row.querySelectorAll('.info .tag')) {
                        if ((tag.textContent || '').trim() === '视频') { tag.click(); return true; }
                    }
                }
                return false;
            }""",
            name,
        )
    except Exception as exc:
        log(f"  点视频失败 {name[:20]}: {str(exc)[:90]}")
        return False

    video = None
    for _ in range(8):
        await page.wait_for_timeout(1_500)
        if await page.locator("video").count():
            video = page.locator("video").first
            break
    if video is None:
        log(f"  {name[:24]}: 没等到播放器")
        return False
    try:
        await video.evaluate(
            "(v, s) => { v.muted = true; v.playbackRate = s; v.play(); }", SPEED
        )
    except Exception as exc:
        log(f"  播放失败 {name[:20]}: {str(exc)[:90]}")
        return False

    deadline = time.monotonic() + min(budget, MAX_VIDEO_SECONDS)
    last = 0.0
    while time.monotonic() < deadline:
        await page.wait_for_timeout(5_000)
        try:
            ended = await video.evaluate("v => v.ended")
            cur = await video.evaluate("v => v.currentTime")
        except Exception:
            break
        if cur - last >= 60:
            last = cur
            log(f"  {name[:24]}: {cur / 60:.1f} 分钟")
        if ended:
            log(f"  ✓ {name[:34]} 播完了 ({cur:.0f}s)")
            return True
    log(f"  · {name[:24]}: 到时限停")
    return False


async def read_progress(page) -> int | None:
    """知识学习页的知识点进度 → 课程级百分比。"""
    try:
        info = await page.evaluate(KNOWLEDGE_PROGRESS_JS)
    except Exception:
        return None
    if not info.get("total"):
        return None
    return int(info.get("percent"))


async def brush_course(page, title: str, course_url: str, max_minutes: int,
                       dry_run: bool) -> dict:
    result = {"title": title, "url": course_url, "watched": [], "failed": []}
    if not await goto_video_page(page, course_url):
        result["error"] = "进不了课程视频页"
        return result

    videos = await list_videos(page)
    log(f"[{title}] 视频条目 {len(videos)} 个")
    if not videos:
        result["error"] = "没找到视频条目"
        return result

    state = load_state()
    done = set(state.get(course_url) or [])
    pending = [v for v in videos if v["name"] not in done]
    log(f"[{title}] 我们已播完 {len(videos) - len(pending)} 个, 待播 {len(pending)} 个")
    for v in pending:
        log(f"   - {v['name'][:40]}")

    if dry_run:
        result["pending"] = [v["name"] for v in pending]
        return result

    budget = max_minutes * 60
    spent = 0.0
    for v in pending:
        left = budget - spent
        if left <= 60:
            log(f"[{title}] 到达时限({max_minutes} 分钟), 停")
            break
        started = time.monotonic()
        ok = await play_video(page, v["name"], left)
        spent += time.monotonic() - started
        (result["watched"] if ok else result["failed"]).append(v["name"])
        if ok:
            done.add(v["name"])
            state[course_url] = sorted(done)
            save_state(state)
    return result


async def main_async() -> int:
    parser = argparse.ArgumentParser(description="ai-smart 课程视频挂机")
    parser.add_argument("--url", action="append", default=[], help="课程主页 URL(可多次)")
    parser.add_argument("--max-minutes", type=int, default=30)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if not args.url:
        log("没有指定 --url")
        return 1

    config = Config(config_path="config.ini")
    cookies = load_cookies(COOKIES_PATH)
    results = []
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            for url in args.url:
                title = url.rstrip("/").rsplit("/", 1)[-1]
                log(f"===== {title} =====")
                try:
                    res = await brush_course(page, title, url, args.max_minutes, args.dry_run)
                except Exception as exc:
                    log(f"{title} 异常: {str(exc)[:150]}")
                    res = {"title": title, "error": str(exc)[:200]}
                results.append(res)
        finally:
            await session.context.browser.close()

    watched = sum(len(r.get("watched") or []) for r in results)
    log(f"===== 本轮播完 {watched} 个视频 =====")
    return 0


def main() -> int:
    return asyncio.run(main_async())


if __name__ == "__main__":
    sys.exit(main())
