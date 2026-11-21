"""探针: 点「课程视频」标签, 看 URL 变成什么、Autovisor 目录选择器命中多少.

背景(2026-11-21): ai-smart 课程的 URL 形如
    /singleCourse/knowledgeStudy/<courseId>/<chapterId>   ← 这是「知识图谱」页
它上面有 span[title] 章节列表(C语言概述…)，但 Autovisor 的 4 类目录选择器
全部命中 0 → detect_catalog 会抛 RuntimeError → 该门课失败 → 整轮 break。

新版 AI 课的**视频课时列表**应该在另一个标签页(「课程视频」)。本探针点它,
把跳转后的 URL 和选择器命中数都打出来 —— 这决定了 config.ini 里该填哪条 URL。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_course_video.py "<课程URL>" ["标签文字"]; kill $XPID 2>/dev/null'
"""
import asyncio
import sys

sys.path.insert(0, "/app")
sys.path.insert(0, "/scripts")

from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

CATALOGS = {
    "wisdom(新版AI课)": ".child-info.hasvideo",
    "fusion(融合课)": ".chapter-item",
    "hike": ".file-item",
    "legacy(旧版)": ".clearfix.video",
    "any-child-info": ".child-info",
}

# 候选标签文字(按页面上真实文案试)
TABS = ["课程视频", "视频课程", "课程目录", "学习视频"]


async def click_tab(page, label: str) -> bool:
    """按可见文字点一个标签; 找不到返回 False."""
    try:
        loc = page.get_by_text(label, exact=False)
        n = await loc.count()
        if not n:
            return False
        await loc.first.click(timeout=8_000)
        return True
    except Exception:
        return False


async def main() -> None:
    url = sys.argv[1]
    wanted = sys.argv[2] if len(sys.argv) > 2 else None
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            print(f"起始 URL: {page.url}", flush=True)
            print(f"标题: {await page.title()}", flush=True)

            labels = [wanted] if wanted else TABS
            for label in labels:
                ok = await click_tab(page, label)
                print(f"点标签 {label!r}: {ok}", flush=True)
                if not ok:
                    continue
                await page.wait_for_timeout(6_000)
                print(f"  点后 URL: {page.url}", flush=True)
                print(f"  点后标题: {await page.title()}", flush=True)
                for name, sel in CATALOGS.items():
                    try:
                        n = await page.locator(sel).count()
                    except Exception:
                        n = "err"
                    print(f"    {name:<18} {sel:<26} 命中 {n}", flush=True)
                n = await page.locator("span[title]").count()
                if n:
                    titles = await page.locator("span[title]").all_text_contents()
                    print(f"    span[title]: {n} 个 → "
                          f"{' | '.join(t.strip()[:18] for t in titles[:8])}", flush=True)
                print("", flush=True)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
