"""ai-smart 新形态(图谱)课刷课 —— 逐个知识点点开资源, 让平台自己点亮「已完成」。

背景(2026-11-24 实测, 见 scripts/probe_aismart_*.py):
    「高等数学I(理)」是**新形态课程**, 跟普通智慧树课结构完全不同:
        主页 /singleCourse/knowledgeStudy/<courseId>/<chapterId>
          ├─ 没有「课程视频」入口(brush_aismart.py 找不到)
          ├─ 「课件资料」tab 写着「共 0 个学习资源」
          └─ 真正的资源挂在**知识点**下:
               点知识点卡片(.item-mask) → /learnPage/<course>/<knowledgeId>/<chapter>
                 ├─ 左侧目录: 56 个知识点, 每条带「必学 n/m」
                 └─ 右侧抽屉「切换学习资源」: 该知识点的资源卡片
                      .basic-info-video-card-container → 「二、函数 已完成」「38页 01-02 函数.pptx」

    点开资源 → 前端自己发 stu/studyRecord/completed → 资源变「已完成」→ 知识点「必学」计数上涨。
    实测: 「函数的概念」必学 0/2 → 2/2(点开 pptx 资源后)。
    ⚠️ 请求体是**加密**的(secretStr), 所以只能走真实浏览路径, 不能伪造上报。

刷法(和 polymas 浏览文档一个思路): 逐个知识点 → 逐个资源 → 点开 → 停留 → 关掉预览。
不做任何伪造上报, 只做人在做的事。

用法(容器内):
    python -u /scripts/brush_aismart_graph.py --url <课程URL> [--max-minutes 40] [--dry-run]
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
STATE_PATH = os.path.join(DATA_DIR, "aismart_graph_watched.json")

# 每个资源点开后停留多久(秒) —— 实测 25 秒内平台就点亮了, 留点余量
DWELL_SECONDS = 20
# 知识点之间切换后等资源面板刷新
KP_SETTLE = 7_000
# 关预览弹层: 依次尝试这些选择器
CLOSE_SELECTORS = (
    "[class*=preview] [class*=close]",
    ".el-dialog__headerbtn",
    "[class*=dialog] [class*=close]",
    "[class*=drawer] [class*=close]",
    "[class*=popup] [class*=close]",
    "[class*=mask] [class*=close]",
)

CAT_JS = """() => {
    const out = [];
    for (const el of document.querySelectorAll('.section-item-collapse-info')) {
        const prog = el.querySelector('.progress-text');
        const mark = prog ? (prog.textContent || '').trim() : '';
        const m = mark.match(/必学\\s*(\\d+)\\s*\\/\\s*(\\d+)/);
        if (!m) continue;
        const clone = el.cloneNode(true);
        clone.querySelectorAll('.collapse-info-progress').forEach(n => n.remove());
        out.push({
            mark: mark,
            done: parseInt(m[1], 10),
            total: parseInt(m[2], 10),
            title: (clone.textContent || '').trim().slice(0, 40),
        });
    }
    return out;
}"""

RES_JS = """() => {
    const root = document.querySelector('.resources-detail-container');
    if (!root) return [];
    return Array.from(root.querySelectorAll('.basic-info-video-card-container')).map((c, i) => {
        const t = (c.innerText || '').trim().replace(/\\s+/g, ' ');
        return {i: i, text: t.slice(0, 90), done: /已完成/.test(t),
                active: /\\bactive\\b/.test(c.className || '')};
    });
}"""

# 点某个知识点(按标题精确匹配)
CLICK_KP_JS = """(title) => {
    const norm = (s) => (s || '').replace(/\\s+/g, '');
    for (const el of document.querySelectorAll('.section-item-collapse-info')) {
        const clone = el.cloneNode(true);
        clone.querySelectorAll('.collapse-info-progress').forEach(n => n.remove());
        if (norm(clone.textContent) !== norm(title)) continue;
        const t = el.querySelector('[class*=title], [class*=name]')
              || Array.from(el.querySelectorAll('*')).find(
                  e => !e.children.length && norm(e.textContent) === norm(title));
        (t || el).click();
        return true;
    }
    return false;
}"""

# 点第 n 个资源卡片
CLICK_RES_JS = """(idx) => {
    const cands = Array.from(document.querySelectorAll('.basic-info-video-card-container'));
    const c = cands[idx];
    if (!c) return false;
    c.click();
    return true;
}"""

CLOSE_JS = """(selectors) => {
    for (const sel of selectors) {
        for (const el of document.querySelectorAll(sel)) {
            const r = el.getBoundingClientRect();
            if (r.width === 0 || r.height === 0) continue;
            el.click();
            return sel;
        }
    }
    return null;
}"""


def log(message: str) -> None:
    print(f"[brush-graph] {message}", flush=True)


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as fh:
            data = json.load(fh)
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def save_state(data: dict) -> None:
    try:
        with open(STATE_PATH, "w", encoding="utf-8") as fh:
            json.dump(data, fh, ensure_ascii=False, indent=1)
    except Exception as exc:
        log(f"状态写盘失败: {exc}")


async def enter_learn_page(page, course_url: str) -> bool:
    """主页 → 点第一个知识点卡片 → 进 learnPage。"""
    await page.goto(course_url, wait_until="domcontentloaded", timeout=60_000)
    for _ in range(16):
        await page.wait_for_timeout(1_500)
        if await page.evaluate("document.body ? document.body.innerText.length : 0") > 200:
            break
    await page.wait_for_timeout(6_000)
    await close_popups(page)
    await page.wait_for_timeout(2_000)
    ok = await page.evaluate(
        """() => {
            const e = document.querySelector('.item-mask') || document.querySelector('.item-content');
            if (!e) return false;
            e.click();
            return true;
        }"""
    )
    await page.wait_for_timeout(10_000)
    log(f"learnPage: {page.url}" if ok else "没找到知识点卡片")
    return bool(ok)


async def close_preview(page) -> str:
    """关掉资源预览弹层。返回用到的选择器(没关掉返回 None)。"""
    used = await page.evaluate(CLOSE_JS, list(CLOSE_SELECTORS))
    if used:
        await page.wait_for_timeout(2_500)
        return used
    # 没有显式关闭按钮 → ESC
    await page.keyboard.press("Escape")
    await page.wait_for_timeout(2_000)
    return "Escape"


async def brush(page, course_url: str, max_minutes: int, dry_run: bool) -> int:
    deadline = time.time() + max_minutes * 60
    state = load_state()
    course_key = course_url.split("/knowledgeStudy/")[-1]

    if not await enter_learn_page(page, course_url):
        return 1

    catalog = await page.evaluate(CAT_JS)
    total_kp = len(catalog)
    done_kp = sum(1 for c in catalog if c["done"] >= c["total"])
    log(f"目录共 {total_kp} 个知识点, 已完成 {done_kp} 个")
    if not catalog:
        log("目录是空的, 退出")
        return 1

    # 逐个知识点刷(每轮重读目录, 状态是最新的)
    index = 0
    while time.time() < deadline:
        catalog = await page.evaluate(CAT_JS)
        pending = [c for c in catalog if c["done"] < c["total"]]
        if not pending:
            log("全部知识点都完成了")
            break

        kp = pending[0]
        index += 1
        log(f"[{index}] 知识点「{kp['title']}」{kp['mark']}")

        clicked = await page.evaluate(CLICK_KP_JS, kp["title"])
        if not clicked:
            log("  点不到这个知识点, 跳过")
            continue
        await page.wait_for_timeout(KP_SETTLE)

        resources = await page.evaluate(RES_JS)
        log(f"  资源 {len(resources)} 个: " +
            "; ".join(f"{r['text']}{'✓' if r['done'] else '✗'}" for r in resources))

        if dry_run:
            continue

        for res in resources:
            if res["done"]:
                continue
            if time.time() >= deadline:
                log("  预算用完, 停")
                break
            ok = await page.evaluate(CLICK_RES_JS, res["i"])
            if not ok:
                continue
            await page.wait_for_timeout(DWELL_SECONDS * 1000)
            used = await close_preview(page)
            # 等平台的进度回写到左侧目录(实测要几秒), 免得下一轮又重刷一遍
            await page.wait_for_timeout(6_000)
            after = await page.evaluate(RES_JS)
            now_done = after[res["i"]]["done"] if res["i"] < len(after) else False
            log(f"  点开「{res['text'][:40]}」→ {'已完成 ✓' if now_done else '还没点亮'}"
                f"（关预览: {used}）")

        # 刷新一下这个知识点的进度, 记进状态
        fresh = await page.evaluate(CAT_JS)
        cur = next((c for c in fresh if c["title"] == kp["title"]), None)
        if cur:
            log(f"  → 现在 {cur['mark']}")
        state.setdefault(course_key, {})[kp["title"]] = {
            "mark": cur["mark"] if cur else kp["mark"],
            "at": time.strftime("%Y-%m-%d %H:%M:%S"),
        }
        save_state(state)

    catalog = await page.evaluate(CAT_JS)
    done_kp = sum(1 for c in catalog if c["done"] >= c["total"])
    log(f"收工: {done_kp}/{len(catalog)} 个知识点完成")
    return 0


async def brush_course(page, title: str, course_url: str, max_minutes: int,
                       dry_run: bool) -> dict:
    """跟 brush_aismart / brush_polymas 一样的入口签名, 便于 brush_courses.py 分发。"""
    result = {"title": title, "url": course_url, "watched": [], "failed": []}
    try:
        code = await brush(page, course_url, max_minutes, dry_run)
    except Exception as exc:  # noqa: BLE001
        log(f"[{title}] 异常: {str(exc)[:200]}")
        result["error"] = str(exc)[:200]
        return result
    if code != 0:
        result["error"] = "刷课未正常收工"
    return result


async def main_async() -> int:
    parser = argparse.ArgumentParser(description="ai-smart 图谱课刷课")
    parser.add_argument("--url", action="append", default=[], help="课程主页 URL(可多次)")
    parser.add_argument("--max-minutes", type=int, default=40)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        code = 0
        for url in args.url:
            log(f"===== {url} =====")
            code = await brush(page, url, args.max_minutes, args.dry_run)
    return code


def main() -> int:
    try:
        return asyncio.run(main_async())
    except KeyboardInterrupt:
        log("中断")
        return 130


if __name__ == "__main__":
    sys.exit(main())
