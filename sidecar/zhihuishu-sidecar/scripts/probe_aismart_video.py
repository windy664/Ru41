"""探针: ai-smart「新版 AI 课」里到底有没有视频可刷。

背景(2026-11-22): 程序设计基础 / 高等数学I(理) 两门课的采集结果是
content=None(探测不到内容) → 刷课目标生成器不会选它们。如果这两门其实有
视频, 就是漏刷。polymas 那边已经有「我的学习 → 学习资源 → 逐个 mp4」的
成熟流程; 这里要摸清 ai-smart 的对应结构:
    知识学习页(knowledgeStudy) → 章节 tab → ??? → 视频列表?

跑法(容器内, 复用 cookies):
    docker run --rm -v "$PWD/config.ini:/app/config.ini:ro" -v "$PWD/data:/app/data" \
      -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
      sh -c 'Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5;
            DISPLAY=:99 python -u /scripts/probe_aismart_video.py "<课程URL>"; kill $XPID 2>/dev/null'
"""
import asyncio
import sys

sys.path.insert(0, "/scripts")

from collector import close_popups  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

DUMP = """() => {
    const out = {};
    out.videos = document.querySelectorAll('video').length;
    out.iframes = document.querySelectorAll('iframe').length;
    out.tabs = Array.from(document.querySelectorAll('.ant-tabs-tab-btn')).map(e => e.textContent.trim());
    // 找像「视频条目」的节点: 名字里带 视频/课程视频/video 的
    out.videoish = Array.from(document.querySelectorAll('*'))
        .filter(e => e.children.length === 0 && /视频|video|Video/.test(e.textContent || ''))
        .slice(0, 12)
        .map(e => (e.textContent || '').trim().slice(0, 40));
    // 常见播放入口
    out.players = Array.from(document.querySelectorAll('[class*=play],[class*=Play],[class*=video],[class*=Video]'))
        .slice(0, 12)
        .map(e => e.tagName + '.' + (e.className || '').toString().slice(0, 50));
    out.text = (document.body ? document.body.innerText : '').slice(0, 700);
    return out;
}"""


async def main() -> None:
    url = sys.argv[1]
    config = Config(config_path="config.ini")
    cookies = load_cookies("/app/data/cookies.json")
    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        page = await session.context.new_page()
        try:
            await page.goto(url, wait_until="domcontentloaded", timeout=60_000)
            await page.wait_for_timeout(6_000)
            await close_popups(page)
            print(f"=== 起始页 {page.url}", flush=True)
            info = await page.evaluate(DUMP)
            print(f"  video={info['videos']} iframe={info['iframes']}", flush=True)
            print(f"  章节 tab: {info['tabs'][:10]}", flush=True)
            print(f"  含'视频'的节点: {info['videoish'][:6]}", flush=True)
            print(f"  播放器类: {info['players'][:6]}", flush=True)

            # 点「课程视频」
            for label in ["课程视频", "视频课程"]:
                loc = page.get_by_text(label, exact=True)
                if await loc.count():
                    try:
                        await loc.first.click(timeout=8_000)
                        await page.wait_for_timeout(6_000)
                        print(f"\n=== 点「{label}」后 {page.url}", flush=True)
                        info = await page.evaluate(DUMP)
                        print(f"  video={info['videos']} iframe={info['iframes']}", flush=True)
                        print(f"  章节 tab: {info['tabs'][:10]}", flush=True)
                        print(f"  含'视频'的节点: {info['videoish'][:6]}", flush=True)
                        print(f"  播放器类: {info['players'][:8]}", flush=True)
                        print(f"  正文前 400 字: {info['text'][:400]!r}", flush=True)
                        break
                    except Exception as exc:
                        print(f"  点 {label} 失败: {str(exc)[:100]}", flush=True)

            # 点第一个章节 tab, 看有没有视频列表
            tabs = page.locator(".ant-tabs-tab-btn")
            if await tabs.count():
                name = (await tabs.first.text_content() or "").strip()[:20]
                try:
                    await tabs.first.click(timeout=8_000)
                    await page.wait_for_timeout(6_000)
                    print(f"\n=== 点第一个章节「{name}」后 {page.url}", flush=True)
                    info = await page.evaluate(DUMP)
                    print(f"  video={info['videos']} iframe={info['iframes']}", flush=True)
                    print(f"  含'视频'的节点: {info['videoish'][:8]}", flush=True)
                    print(f"  播放器类: {info['players'][:8]}", flush=True)
                except Exception as exc:
                    print(f"  点章节失败: {str(exc)[:100]}", flush=True)
        finally:
            await page.close()
            await session.context.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
