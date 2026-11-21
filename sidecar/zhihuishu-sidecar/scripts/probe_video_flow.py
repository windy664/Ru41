"""探针: 走完「新版 AI 课」的点击流, 找课时列表(Autovisor 能刷的那页).

背景(2026-11-21): Autovisor 首跑在 ai-smart 课程上直接失败:
    RuntimeError: 课程目录加载超时，且未检测到可处理的安全验证
detect_catalog 认的是 DOM 选择器(.child-info.hasvideo 等), 全不命中。

已知:
- knowledgeStudy 页 = 知识图谱(ant-tabs 章节 + 知识点进度环), 无 <video>
- 点「课程视频」→ URL 变成 singleCourse/courseVideoShare/<id>/<id>?mapUid=...
  但那一页当时也没渲染出课时列表。

本探针按真实操作顺序走一遍, 每一步都数一次:
    课程视频标签 → 章节 tab → 展开 → <video> / 目录选择器
最后给出「Autovisor 的 URL 该填哪条 + 需不需要先点哪里」的结论。

跑法:
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_video_flow.py "<课程URL>"; kill $XPID 2>/dev/null'
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
    "wisdom": ".child-info.hasvideo",
    "fusion": ".chapter-item",
    "hike": ".file-item",
    "legacy": ".clearfix.video",
}


async def report(page, label: str) -> None:
    print(f"\n--- {label}", flush=True)
    print(f"    URL: {page.url}", flush=True)
    for name, sel in CATALOGS.items():
        try:
            n = await page.locator(sel).count()
        except Exception:
            n = "err"
        if n:
            print(f"    {name:<8} {sel:<26} 命中 {n}", flush=True)
    vids = await page.locator("video").count()
    print(f"    <video>: {vids}", flush=True)
    if vids:
        for i in range(min(vids, 3)):
            v = page.locator("video").nth(i)
            src = await v.get_attribute("src")
            print(f"      video[{i}] src={str(src)[:100]}", flush=True)
    tabs = await page.locator("span[title]").count()
    if tabs:
        names = await page.locator("span[title]").all_text_contents()
        print(f"    span[title]: {tabs} → "
              f"{' | '.join(t.strip()[:16] for t in names[:8])}", flush=True)
    ant = await page.locator(".ant-tabs-tab-btn").count()
    if ant:
        names = await page.locator(".ant-tabs-tab-btn").all_text_contents()
        print(f"    ant-tabs-tab-btn: {ant} → "
              f"{' | '.join(t.strip()[:16] for t in names[:8])}", flush=True)
    items = await page.locator(".course-item, .resource-item, .file-item").count()
    if items:
        print(f"    资源条目(course-item/resource-item/file-item): {items}", flush=True)


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(8_000)
            await report(page, "起始页")

            # 1) 点「课程视频」
            for label in ["课程视频", "视频"]:
                loc = page.get_by_text(label, exact=False)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=8_000)
                        await page.wait_for_timeout(7_000)
                        await report(page, f"点了「{label}」")
                        break
                    except Exception as exc:
                        print(f"  点 {label} 失败: {str(exc)[:80]}", flush=True)

            # 2) 点第一个章节 tab
            for sel in [".ant-tabs-tab-btn", ".chapter-item", ".course-item"]:
                loc = page.locator(sel)
                if await loc.count():
                    try:
                        name = (await loc.first.text_content() or "").strip()[:16]
                        await loc.first.click(timeout=8_000)
                        await page.wait_for_timeout(7_000)
                        await report(page, f"点了第一个 {sel}({name})")
                        break
                    except Exception as exc:
                        print(f"  点 {sel} 失败: {str(exc)[:80]}", flush=True)

            # 3) 看有没有播放按钮
            for sel in [".el-button--primary", "button:has-text('播放')", ".play-icon"]:
                n = await page.locator(sel).count()
                if n:
                    print(f"\n  播放类按钮 {sel}: {n}", flush=True)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
