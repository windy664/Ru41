"""正式采集器: 登录校验(失效自动账密重登) → 逐课程卡片采集任务 → report.json.

红线: 只读采集. 只浏览课程主页/任务列表, 绝不点击「开始答题」及之后的提交类按钮,
绝不进入验证页之后. 考试式任务(限时/监考)只采集元数据, 永不自动作答.

报告契约见 Ru41 仓库 docs/sources-zhihuishu.md; 消费端 src/modules/zhihuishu.

用法(容器, Xvfb 已拉起): python -u /scripts/collector.py [--out /app/data/report.json]
课程卡片识别失败时可手动指定: ... --courses "职业生涯规划,军事理论"
"""
import argparse
import asyncio
import json
import os
import re
import sys
import time
from datetime import datetime, timedelta, timezone
from urllib.parse import urlparse

sys.path.insert(0, "/app")
# scripts/ 自己目录: 纯函数模块(course_state), 宿主上没有 /app 也能 import
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import course_state  # noqa: E402
from modules import login as login_mod  # noqa: E402
from modules import slider as slider_mod  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies, save_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()

# 上游 Autovisor 的 4 类课程目录选择器(modules/lesson_navigation.py)。
# 认课靠 DOM 不靠域名, 四类都不命中 → 该门课失败 → 整轮 break。
# 见 detect_autovisor_catalog()。
AUTOVISOR_CATALOGS = {
    "wisdom": ".child-info.hasvideo",
    "fusion": ".chapter-item",
    "hike": ".file-item",
    "legacy": ".clearfix.video",
}
PORTAL = "https://onlineweb.zhihuishu.com"
# 全局待办聚合页(跨课程/跨平台 pending 视图; 背后是 student-course/stu/todo/findNoFinishTaskTotal)
STUDY_TASK_URL = "https://hike-teaching-center.polymas.com/stu-hike/studyTask"
LIST_API = "activityTask/list"
LOGIN_HOSTS = {"login.zhihuishu.com", "passport.zhihuishu.com"}

try:  # 滑块求解依赖 OpenCV; 缺失时仍可账密登录(仅无滑块场景)
    import cv2  # noqa: E402
    import numpy as np  # noqa: E402

    CV_MODULES = [np, cv2]
except Exception:
    CV_MODULES = None

STEP_TIMEOUT = 30_000
COURSE_TIMEOUT = 150.0
# 详情抓取单任务预算; 失败降级不影响整轮
DETAIL_TIMEOUT = 45.0
# 学期 ID(详情页 URL 参数); 每学期手动更新
TERM_ID = "20271"
# 附件落盘目录(相对 data 根)
ATTACHMENTS_DIR = "attachments"
# 容器内数据根(附件与报告落盘处)
DATA_DIR = "/app/data"

CARDS_JS = """() => {
  const BADGE = /^(AI智课|翻转课|图谱|共享课|SPOC)$/;
  const norm = (s) => (s || '').replace(/20\\d{2}[-/年].*学期/g, '').trim();
  return [...document.querySelectorAll('.course-card')].map(card => {
    let title = '';
    const nameEl = card.querySelector('[class*="name" i], [class*="title" i]');
    if (nameEl) title = norm((nameEl.textContent || '').split('\\n')[0]);
    const lines = (card.innerText || '').split('\\n').map(norm).filter(Boolean);
    if (!title) {
      title = lines.find(l => !BADGE.test(l) && !l.includes('、')) || '';
    }
    const badge = lines.find(l => BADGE.test(l)) || '';
    return {title, badge};
  }).filter(c => c.title);
}"""

CLICK_NTH_CARD_JS = """(i) => {
  const cards = document.querySelectorAll('.course-card');
  if (i >= cards.length) return false;
  cards[i].click();
  return true;
}"""

CLICK_LEAF_JS = """(label) => {
  const hit = [...document.querySelectorAll('*')]
    .find(e => e.childElementCount === 0 && e.textContent.trim() === label);
  if (hit) { hit.click(); return true; }
  return false;
}"""


def now_ts() -> str:
    return datetime.now().strftime("%Y-%m-%d %H:%M:%S")


# 平台时间是北京时间(UTC+8)。sidecar 容器没设 TZ(playwright 基础镜像是 UTC),
# datetime.now() 拿到的是 UTC——epoch 换算绝不能用容器本地时区, 会差 8 小时。
# 中国自 1991 年起不再使用夏令时, 固定 +8 与 Asia/Shanghai 完全等价。
CST = timezone(timedelta(hours=8))

# epoch 合理区间: 2001-09-09 ~ 2100-01-01。区间外的纯数字(如 "20261114" 这种
# 紧凑日期)不当 epoch 处理, 原样透传, 避免把日期串误解析成 1970 年的时间。
_EPOCH_MIN = 1_000_000_000
_EPOCH_MAX = 4_102_444_800_000


def normalize_time(value) -> str | None:
    """平台时间 → 契约格式 "YYYY-MM-DD HH:MM:SS"（北京时间）。

    为什么需要：智慧树的「全局待办」接口(findNoFinishTaskTotal)的
    startTime/endTime 返回的是毫秒 epoch（如 "1794628800000"），而课程流返回的是
    平台已格式化好的 ISO 串("2026-11-14 12:00:00")——两种形态不同源。
    不归一化就混在一起比字符串, 会误报「截止变更」，并且下游 parse_platform_time
    只认 ISO 串, 解析失败会让截止/临期判定与待办 due_at 整体失效。

    支持: 毫秒 epoch(int/str)、秒 epoch(int/str)、ISO 串(原样透传)。
    无法识别时原样返回字符串(不丢数据); None/空串 → None。
    """
    if value is None:
        return None
    if isinstance(value, bool):
        return str(value)
    if isinstance(value, (int, float)):
        raw = int(value)
    else:
        text = str(value).strip()
        if not text:
            return None
        if not text.isdigit():
            return text  # 已是 ISO 串等, 透传
        raw = int(text)
    if not (_EPOCH_MIN <= raw <= _EPOCH_MAX):
        return str(value)
    seconds = raw // 1000 if raw >= 100_000_000_000 else raw
    try:
        return datetime.fromtimestamp(seconds, tz=CST).strftime("%Y-%m-%d %H:%M:%S")
    except (OverflowError, OSError, ValueError):
        return str(value)


def platform_of(url: str) -> str:
    host = (urlparse(url).hostname or "").lower()
    if "wenda" in host:
        return "wenda"
    if "polymas" in host or "ai-smart-course" in host:
        return "polymas"  # AI智课家族: hike-teaching-center 与 ai-smart-course 学生端
    return "?"


async def js_click(page, label: str) -> bool:
    """点击文本精确匹配的叶子元素(探针同款, 避免命中隐藏节点)."""
    try:
        return await asyncio.wait_for(
            page.evaluate(CLICK_LEAF_JS, label), timeout=STEP_TIMEOUT / 1000
        )
    except Exception:
        return False


async def close_popups(page) -> None:
    for label in ("我知道了", "确定", "知道了"):
        try:
            await asyncio.wait_for(page.evaluate(CLICK_LEAF_JS, label), timeout=3)
            await page.wait_for_timeout(600)
        except Exception:
            pass


async def goto_portal(page) -> bool:
    """回门户; 失败只记日志返回 False, 由调用方决定降级."""
    try:
        await page.goto(PORTAL, wait_until="domcontentloaded", timeout=90_000)
        return True
    except Exception as exc:
        print(f"[courses] 打开门户失败: {str(exc)[:120]}", flush=True)
        return False


async def wait_for_cards(page, attempts: int = 15, gap_ms: int = 2000) -> list:
    """自适应等课程卡渲染: 慢网最多等 attempts*gap_ms, 快网首轮即返回."""
    for _ in range(attempts):
        try:
            cards = await page.evaluate(CARDS_JS)
        except Exception:
            cards = []
        if cards:
            return cards
        await page.wait_for_timeout(gap_ms)
    return []


async def ensure_login(session, config, cookie_path: str) -> bool:
    """Cookies 有效即通过; 失效则账密重登(滑块自动求解), 成功后刷新 cookie 文件."""
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
        save_cookies(await context.cookies(), cookie_path)  # 同步函数, 不要 await
        print("[login] 重登成功, cookies 已更新", flush=True)
    else:
        print("[login] 重登失败(可能滑块需人工)", flush=True)
    return ok


def completed_state(raw: dict):
    """完成态三态化: True=已完成 / False=未完成 / None=接口没给完成态(未知).

    polymas 的 activityTask/list 正常下发 isCompleted; 字段缺失时必须报「未知」,
    不能默认 False——消费端会把「已完成→未完成」当成「老师打回/开放补交」,
    重建待办并推送提醒(2026-11-13 生产实况: 军事理论作业被反复重建待办).
    """
    for key in ("isCompleted", "completed", "isFinish", "finished"):
        if key not in raw:
            continue
        value = raw.get(key)
        if value is None:
            return None
        if isinstance(value, str):
            return value.strip().lower() in ("1", "true", "yes", "是", "已完成")
        return bool(value)
    return None


def to_contract(raw: dict) -> dict:
    """平台任务字段 → report 契约字段(docs/sources-zhihuishu.md)."""
    exam_duration = raw.get("examDuration")
    invigilate = raw.get("videoInvigilate")
    is_exam_style = bool(exam_duration) or invigilate is True
    type_code = raw.get("type")
    if is_exam_style:
        kind = "exam"
    elif type_code == 5:
        kind = "assignment"
    elif type_code == 6:
        kind = "group"
    else:
        kind = "other"
    start = raw.get("startTime")
    if not start:
        times = raw.get("classTaskTimeList") or []
        start = (times[0] or {}).get("startTime") if times else None
    return {
        "activity_task_id": str(raw.get("activityTaskId") or ""),
        "title": str(raw.get("title") or "").strip(),
        "kind": kind,
        "course_id": raw.get("courseId"),
        "group_id": raw.get("groupId"),
        "start_at": normalize_time(start),
        "end_at": normalize_time(raw.get("endTime")),
        # 三态: True=已完成 / False=未完成 / None=接口未给完成态(未知)。
        # 未知不能当成未完成——那会让消费端误判「老师打回」并重建待办。
        "completed": completed_state(raw),
        "total_score": raw.get("totalScore"),
        "exam_duration_minutes": exam_duration if exam_duration else None,
        "video_invigilate": invigilate if invigilate else None,
        "late_submit": None,  # 列表 API 不下发, 详情页才有; 留空
        "content": (raw.get("content") or "").strip() or None,
    }


def detail_url_for(task: dict) -> str | None:
    """拼接作业详情页 URL; 缺 course_id 的任务返回 None."""
    course_id = task.get("course_id")
    task_id = task.get("activity_task_id")
    if not course_id or not task_id:
        return None
    url = (
        "https://hike-teaching-center.polymas.com/exam-hub/"
        f"{course_id}/stu/homework/detail/{task_id}?termId={TERM_ID}"
    )
    if task.get("group_id"):
        url += f"&groupId={task['group_id']}"
    return url


def find_attachments(node, found: list) -> None:
    """递归在详情 API JSON 里找附件项(名称 + URL 的对象)."""
    if isinstance(node, dict):
        keys = set(node.keys())
        name = next(
            (node[k] for k in ("fileName", "name", "title")
             if k in keys and isinstance(node[k], str)),
            None,
        )
        url = next(
            (node[k] for k in ("ossUrl", "fileUrl", "url", "fileDownloadUrl", "downUrl")
             if k in keys and isinstance(node[k], str) and node[k].startswith("http")),
            None,
        )
        if name and url:
            found.append({"name": name, "url": url})
        for value in node.values():
            find_attachments(value, found)
    elif isinstance(node, list):
        for item in node:
            find_attachments(item, found)


async def scrape_task_detail(page, task: dict, data_dir: str) -> dict:
    """只读作业详情页: 截详情 API 拿附件并下载到 data/attachments/{taskId}/.

    红线: 只读文本与网络响应, 绝不点击「开始答题」等任何提交入口;
    考试式任务在调用侧整体跳过.
    幂等: 附件目录已存在则直接复用, 不再访问平台; 删除目录即可强制刷新.
    """
    task_id = task.get("activity_task_id") or ""
    attach_dir = os.path.join(data_dir, ATTACHMENTS_DIR, task_id)
    result: dict = {"attachments": None, "late_submit": None, "content_detail": None}

    def list_files() -> list[str]:
        if not os.path.isdir(attach_dir):
            return []
        return sorted(
            f for f in os.listdir(attach_dir)
            if f not in ("detail_api.json", "meta.json")
            and os.path.isfile(os.path.join(attach_dir, f))
        )

    # 幂等快路径: 已抓过, 从盘上复用
    if list_files():
        files = list_files()
        result["attachments"] = [
            {"name": f, "file": f"{ATTACHMENTS_DIR}/{task_id}/{f}"} for f in files
        ]
        meta_path = os.path.join(attach_dir, "meta.json")
        if os.path.isfile(meta_path):
            try:
                with open(meta_path, encoding="utf-8") as fh:
                    meta = json.load(fh)
                result["late_submit"] = meta.get("late_submit")
                result["content_detail"] = meta.get("content_detail")
            except Exception:
                pass
        return result

    url = detail_url_for(task)
    if not url:
        return result

    async def matcher(resp):
        low = resp.url.lower()
        # 详情载荷在 student-homework 域(小组题 groupHomeworkDetail / 普通作业 detail)
        if "student-homework" in low or "attachment" in low:
            try:
                return await resp.text(), None
            except Exception:
                return None, None
        return None, None

    handle = CollectHandle(page, matcher)
    await handle.start()
    try:
        await page.goto(url, timeout=STEP_TIMEOUT, wait_until="domcontentloaded")
        await page.wait_for_timeout(8_000)
        text = await asyncio.wait_for(
            page.evaluate("document.body ? document.body.innerText.slice(0, 3000) : ''"),
            timeout=STEP_TIMEOUT / 1000,
        )
    except Exception as exc:
        print(f"[detail] {str(task.get('title') or '')[:20]}: 打开详情失败 {str(exc)[:80]}", flush=True)
        return result
    finally:
        handle.stop()

    # 详情载荷结构化字段: requirements(作业要求) / isDelay(迟交) / 附件列表
    requirements = None
    is_delay = None
    found: list = []
    seen_urls: set = set()
    for cap in handle.captured:
        try:
            payload = json.loads(cap["body"])
        except Exception:
            continue
        data = payload.get("data") if isinstance(payload, dict) else None
        if isinstance(data, dict):
            if requirements is None:
                text_req = str(data.get("requirements") or "").strip()
                desc = str(data.get("description") or "").strip()
                requirements = (text_req or desc) or None
            if is_delay is None and data.get("isDelay") is not None:
                is_delay = bool(data.get("isDelay"))
        find_attachments(payload, found)
    # 文本兜底(结构化字段缺失时)
    if is_delay is None:
        if "禁止迟交" in text:
            result["late_submit"] = False
        elif "允许迟交" in text or "可迟交" in text:
            result["late_submit"] = True
    else:
        result["late_submit"] = is_delay
    if requirements:
        result["content_detail"] = requirements

    os.makedirs(attach_dir, exist_ok=True)
    if handle.captured:
        with open(os.path.join(attach_dir, "detail_api.json"), "w", encoding="utf-8") as fh:
            json.dump([c["body"] for c in handle.captured], fh, ensure_ascii=False)

    for item in found:
        if item["url"] in seen_urls:
            continue
        seen_urls.add(item["url"])
        filename = re.sub(r'[\\/:*?"<>|]', "＿", str(item["name"])) or "attachment.bin"
        dest = os.path.join(attach_dir, filename)
        if os.path.exists(dest):
            continue
        try:
            resp = await page.request.get(item["url"], timeout=STEP_TIMEOUT)
            if resp.ok:
                with open(dest, "wb") as fh:
                    fh.write(await resp.body())
            else:
                print(f"[detail] 附件 HTTP {resp.status}: {filename[:40]}", flush=True)
        except Exception as exc:
            print(f"[detail] 附件下载失败 {filename[:40]}: {str(exc)[:60]}", flush=True)

    with open(os.path.join(attach_dir, "meta.json"), "w", encoding="utf-8") as fh:
        json.dump(
            {"late_submit": result["late_submit"], "content_detail": result.get("content_detail")},
            fh,
            ensure_ascii=False,
        )

    files = list_files()
    result["attachments"] = [
        {"name": f, "file": f"{ATTACHMENTS_DIR}/{task_id}/{f}"} for f in files
    ] or None
    return result


class CollectHandle:
    """可移除的响应采集器."""

    def __init__(self, page, matcher):
        self.page = page
        self.matcher = matcher
        self.captured: list = []
        self._cb = None

    async def start(self):
        async def on_response(resp):
            try:
                if "json" not in (resp.headers.get("content-type") or ""):
                    return
                body, req = await self.matcher(resp)
                if body is None:
                    return
                self.captured.append({"body": body, "req": (req or "")[:300]})
            except Exception:
                pass

        self._cb = on_response
        self.page.on("response", on_response)

    def stop(self):
        if self._cb:
            self.page.remove_listener("response", self._cb)


async def scan_polymas_tasks(page) -> tuple[list, int]:
    """在已打开的 polymas 课程页上: 关弹窗 → 我的任务 → 合并 activityTask/list."""

    async def matcher(resp):
        if LIST_API not in resp.url:
            return None, None
        return await resp.text(), resp.request.post_data

    handle = CollectHandle(page, matcher)
    await handle.start()
    try:
        # 慢网下 SPA 可能尚未就绪, 点了没反应就多试几次
        for _ in range(3):
            await close_popups(page)
            if await js_click(page, "我的任务"):
                await page.wait_for_timeout(8_000)
            if handle.captured:
                break
            await page.wait_for_timeout(4_000)
    finally:
        handle.stop()

    merged: dict[str, dict] = {}
    for item in handle.captured:
        try:
            payload = json.loads(item["body"])
            tasks = payload.get("data", {}).get("activityTaskList") or []
        except Exception:
            continue
        for raw in tasks:
            task_id = str(raw.get("activityTaskId") or "")
            if task_id:
                merged.setdefault(task_id, raw)  # 首见优先; 各筛选视图字段一致
    if handle.captured and handle.captured[0]["req"]:
        print(f"[api] {LIST_API} 请求参数样本: {handle.captured[0]['req'][:200]}", flush=True)
    return [to_contract(raw) for raw in merged.values()], len(handle.captured)


def parse_course_state(text: str) -> tuple[bool | None, int | None]:
    """课程页文本 → (content_published, progress_percent).

    纯函数(好测), 页面长什么样由调用方负责取。
    实现见 scripts/course_state.py —— 那里还带着踩坑记录。
    """
    return course_state.parse_course_state(text)


async def detect_autovisor_catalog(page) -> str | None:
    """数一数上游 Autovisor 那 4 类课程目录选择器, 返回命中的目录名(都没命中→None).

    上游认课**不靠域名, 靠 DOM**(modules/lesson_navigation.py):
        wisdom = .child-info.hasvideo / fusion = .chapter-item
        hike   = .file-item         / legacy = .clearfix.video
    四类都不命中 → detect_catalog 抛 RuntimeError「课程目录加载超时」
    → 该门课失败 → **整轮 break**(后面所有课一节都刷不到)。

    所以「这门课 Autovisor 刷不刷得了」只能这么量, 不能看域名猜。
    2026-11-21 实测: 我们门户上的 9 门课在两个平台, 四类选择器**全部命中 0**
    (polymas 的资源列表是 .file-name / .el-tree-node__content, 结构完全不同)。

    返回的目录名写进报告的 catalog 字段, 刷课目标生成器据此挑课
    (见 scripts/plan_targets.py: classify_engine)。
    """
    for name, selector in AUTOVISOR_CATALOGS.items():
        try:
            if await page.locator(selector).count():
                return name
        except Exception:
            continue
    return None


async def detect_catalog_via_video_tab(page) -> str | None:
    """新版 AI 课: 课时列表在「课程视频」标签页, 不在知识图谱页 —— 点过去再数一次."""
    try:
        loc = page.get_by_text("课程视频", exact=True)
        if not await loc.count():
            return None
        await loc.first.click(timeout=8_000)
        await page.wait_for_timeout(6_000)
    except Exception:
        return None
    return await detect_autovisor_catalog(page)


async def detect_aismart_state(page) -> tuple[bool | None, int | None]:
    """ai-smart 新版 AI 课(智慧树)的「有内容没」与进度。

    页面结构与 polymas 完全不同(2026-11-22 生产机实测):
    - 知识学习页(knowledgeStudy): 知识点树, 每个知识点带「掌握度 / 学习进度 X%」
      → 课程级进度 = 已学知识点 / 总知识点
    - 「课程视频」页(courseVideoShare): 章节列表, 带「视频」标签的行 = 可刷的视频

    ⚠️ 还有第三种形态 —— **新形态(图谱)课**(2026-11-24 实测, 「高等数学I(理)」):
      主页**没有**「课程视频」入口, 「课件资料」tab 写着「共 0 个学习资源」,
      但资源挂在**知识点**下(点知识点卡片 → /learnPage → 左侧 56 个知识点带「必学 n/m」)。
      所以「没有课程视频」不等于「没内容」→ 要再探一次 learnPage(见下面第 3 步)。

    返回 (content_published, progress_percent):
    - content_published: True = 有内容可刷; False = 没有; None = 判断不了
    - progress: 0-100 的完成度; None = 读不到
    """
    # 1) 进度: 知识学习页的知识点「学习进度」
    progress: int | None = None
    try:
        text = await page.evaluate(
            "document.body ? document.body.innerText.slice(0, 20000) : ''"
        )
        pcts = [int(m) for m in re.findall(r"学习进度\s*(\d+)%", text)]
        if pcts:
            progress = round(100 * sum(1 for p in pcts if p > 0) / len(pcts))
    except Exception:
        progress = None
    # 2) 内容: 点「课程视频」数视频条目
    content: bool | None = None
    try:
        loc = page.get_by_text("课程视频", exact=True)
        if await loc.count():
            await loc.first.click(timeout=10_000)
            await page.wait_for_timeout(6_000)
            rows = await page.evaluate(
                """() => {
                    let n = 0;
                    for (const row of document.querySelectorAll('.row')) {
                        // 一行可能有多个标签(如 ['1个知识点','视频']), 要全看
                        const tags = Array.from(row.querySelectorAll('.info .tag'))
                            .map(t => (t.textContent || '').trim());
                        if (tags.includes('视频')) n++;
                    }
                    return n;
                }"""
            )
            content = int(rows) > 0
    except Exception:
        content = None
    # 3) 图谱课(新形态): 没有「课程视频」入口时, 再探 learnPage 的知识点目录
    #    (2026-11-24 实测: 「高等数学I(理)」没有课程视频, 但资源挂在知识点下)
    if content is None:
        try:
            await page.evaluate(
                """() => {
                    const e = document.querySelector('.item-mask')
                           || document.querySelector('.item-content');
                    if (e) e.click();
                }"""
            )
            await page.wait_for_timeout(10_000)
            pairs = await page.evaluate(
                """() => {
                    const out = [];
                    for (const el of document.querySelectorAll('.section-item-collapse-info')) {
                        const p = el.querySelector('.progress-text');
                        const m = p ? (p.textContent || '').match(/必学\\s*(\\d+)\\s*\\/\\s*(\\d+)/) : null;
                        if (m) out.push([parseInt(m[1], 10), parseInt(m[2], 10)]);
                    }
                    return out;
                }"""
            )
            if pairs:
                content = True
                total = sum(t for _, t in pairs)
                done = sum(min(d, t) for d, t in pairs)
                progress = round(100 * done / total) if total else 0
        except Exception:
            pass
    return content, progress


async def detect_course_state(page) -> tuple[bool | None, int | None]:
    """点「我的学习」探测课程视频内容是否已发布 + 已学进度.

    返回 (content_published, progress_percent):
    - content_published: None = 无法判断(页面没开/超时); False = 暂无内容; True = 有章节/视频痕迹.
    - progress_percent: 该课「总学习进度」的百分比; None = 读不到.

    两件事在同一个页面同一段文本里, 一次点开一起读(2026-11-21):
    页面形如「总学习进度 42%共 7 个学习资源」。

    ⚠️ 判断「有没有内容」只能用**资源数**, 不能用「学习进度」这类宽松词 ——
    「总学习进度」恒在(哪怕是 0%), 拿它判断内容会假阳性(2026-10 实测踩过).
    但**读进度值本身**是另一回事: 刷课目标生成器要靠它判断「这门还剩多少」.

    进度是**课程整体**的学习进度, 不是当前课时的播放进度(后者是 Autovisor 打印的
    进度条, 只进 stdout, 不落文件)。
    """
    try:
        # 「我的学习」有时还没渲染出来(慢网/刚重登过 cookies) → 点不到。
        # 点不到一次就降级成 None 的代价: 这门课会被自动刷课队列**整个排除**
        # (2026-11-24 实测「26-27-1思想道德与法治」两次都没点着, 而单独手动探测
        #  稳定读到 (True, 100) —— 判据没问题, 只是那一次没点着)。
        # 所以点不到就再等一轮重试, 别急着放弃。
        clicked = False
        for _ in range(2):
            if await js_click(page, "我的学习"):
                clicked = True
                break
            await page.wait_for_timeout(3_000)
        if not clicked:
            return None, None
        await page.wait_for_timeout(6_000)
        # 「总学习进度 X%」里的 X 是**异步取值**、可能比资源数晚几秒渲染；
        # 资源数是页面自带属性、先出来。2026-11-21 实测: 进度在点开后 2s 就有了,
        # 但**必须等内容和进度都读到**才停 —— 只等资源数会让进度永远是 None
        # (采集器写盘时被丢掉过一次, 见 build_report_entry 的注释)。
        # 最多 6 次读取(约 15s 额外等待), 读完为止。
        content: bool | None = None
        progress: int | None = None
        for _ in range(6):
            text = await asyncio.wait_for(
                page.evaluate(
                    "document.body ? document.body.innerText.slice(0, 8000) : ''"
                ),
                timeout=STEP_TIMEOUT / 1000,
            )
            content, progress = parse_course_state(text)
            if content is not None and progress is not None:
                break
            await page.wait_for_timeout(3_000)
    except Exception:
        return None, None
    return content, progress


def wenda_pick(item: dict, *keys: str):
    """防御式取字段: wenda 列表条目字段名无法预先确认, 逐个候选键试探."""
    for k in keys:
        v = item.get(k)
        if v not in (None, ""):
            return v
    return None


def wenda_completed_state(item: dict):
    """wenda(老平台)完成态三态化: True/False/None(未知).

    活样本(homeworkUser/List, 军事理论 第一次线上作业, 2026-11-13 实测):
        {"status":4, "answerScore":85.0, "homeworkScore":100.0, "isShowScore":1,
         "isComplete":"0", "isHomeworkRedo":1, "submitDate":1787491902000}
    平台页面把这条作业标成「已批阅」, 分数 85 —— 学生确实交了。
    但 wenda 根本没有 isCompleted/completed/isFinish/finished 这几个字段,
    旧映射只看 status 并把 status==1 当完成: 交作业后 status=1(已提交待批阅) 判对,
    老师批完 status=4(已批阅) 反而判成「未完成」→ 消费端当成「老师打回」重建待办。
    isComplete="0" 更不可信(已批阅的作业它也报 0), 一律不采信。

    判定顺序(证据从强到弱):
      ① answerScore / isShowScore 有值 → 已批阅 → True
      ② status: 1=已提交(待批阅) 4=已批阅 → True; 0=未提交 → False; 其他 → None
      ③ 都没有 → None(未知)
    """
    answer_score = wenda_pick(item, "answerScore", "homeworkAnswerScore")
    if answer_score is not None:
        return True
    show_score = wenda_pick(item, "isShowScore")
    if show_score is not None:
        if isinstance(show_score, str):
            return show_score.strip() in ("1", "true", "True")
        return bool(show_score)
    status = wenda_pick(item, "status", "completeStatus", "state")
    if status is None:
        return None
    if isinstance(status, str):
        status = status.strip()
    if status in (1, "1", 4, "4", "已完成", True, "finish", "finished"):
        return True
    if status in (0, "0", "未提交", False):
        return False
    return None


def wenda_item_to_contract(item: dict, kind: str) -> dict | None:
    """wenda 作业/考试条目 → 契约字段; 提取不到标题的条目放弃并留档."""
    title = wenda_pick(item, "homeworkTitle", "examTitle", "title", "name")
    if not title:
        return None
    end = wenda_pick(item, "endTime", "homeworkEndTime", "examEndTime", "endDate", "deadline")
    start = wenda_pick(item, "startTime", "homeworkStartTime", "examStartTime", "startDate")
    item_id = wenda_pick(item, "homeworkId", "examId", "id", "hwId")
    return {
        "activity_task_id": str(item_id or ""),
        "title": str(title).strip(),
        "kind": kind,
        "course_id": None,
        "group_id": None,
        "start_at": normalize_time(start),
        "end_at": normalize_time(end),
        "completed": wenda_completed_state(item),
        "late_submit": None,
        "total_score": wenda_pick(item, "homeworkScore", "totalScore", "score"),
        "content": wenda_pick(item, "content", "description", "requirement"),
        "content_published": None,
    }

async def scan_wenda_tasks(page) -> tuple[list, int]:
    """老平台: 点作业考试 tab, 解析考试/作业列表 API.

    红线: 只读列表接口的响应, 绝不点击任何答题/考试入口.
    条目字段名未经活样本校准(当前学期列表暂空), 防御式映射;
    首个真实条目会原文留档 data/wenda_raw.jsonl 供校准.
    """
    async def matcher(resp):
        low = resp.url.lower()
        if not any(k in low for k in ("exam", "homework", "work", "assignment")):
            return None, None
        return await resp.text(), str(resp.url)

    handle = CollectHandle(page, matcher)
    await handle.start()
    try:
        clicked = await js_click(page, "作业考试")
        await page.wait_for_timeout(5_000)

        def has_list_hits() -> bool:
            return any(
                "nowexam" in (cap.get("req") or "").lower()
                or "homeworkuser" in (cap.get("req") or "").lower()
                for cap in handle.captured
            )

        # 门户跳转后的课程页可能有引导弹窗挡住标签点击: 关掉再点一次
        if not has_list_hits():
            await js_click(page, "我知道了")
            clicked = (await js_click(page, "作业考试")) or clicked
            await page.wait_for_timeout(6_000)
    finally:
        handle.stop()

    tasks: list = []
    for cap in handle.captured:
        try:
            payload = json.loads(cap["body"])
        except Exception:
            continue
        rt = payload.get("rt") if isinstance(payload, dict) else None
        if not isinstance(rt, dict):
            continue
        url_low = cap.get("req") or ""
        kind = "exam" if "examl" in url_low or "nowexam" in url_low else "assignment"
        for item in rt.get("list") or []:
            if not isinstance(item, dict):
                continue
            task = wenda_item_to_contract(item, kind)
            if task is None:
                with open("/app/data/wenda_raw.jsonl", "a", encoding="utf-8") as fh:
                    fh.write(json.dumps({"ts": time.strftime("%Y-%m-%d %H:%M:%S"),
                                         "kind_hint": kind, "raw": item}, ensure_ascii=False) + "\n")
                continue
            tasks.append(task)
    return tasks, len(handle.captured)


async def collect_course(portal, index: int) -> dict | None:
    """点击第 index 张课程卡, 在打开的页面上就地采集; 返回 course+tasks."""
    before = {id(p) for p in portal.context.pages}
    card_info = (await portal.evaluate(CARDS_JS))[index]
    clicked = await portal.evaluate(CLICK_NTH_CARD_JS, index)
    if not clicked:
        return None

    target = None
    for _ in range(24):  # 最多等 12s 出现新页或本页跳转
        await portal.wait_for_timeout(500)
        for p in portal.context.pages:
            url = p.url or ""
            if id(p) in before or url.startswith("chrome-error"):
                continue  # 跳过点击前已有页与浏览器错误页
            if not any(h in (urlparse(url).hostname or "") for h in LOGIN_HOSTS):
                target = p
                break
        if target is None and portal.url != PORTAL and "onlineweb" not in portal.url:
            target = portal
        if target is not None:
            try:
                await target.wait_for_timeout(4_000)
            except Exception:
                pass
            break
    if target is None:
        print(f"[collector] {card_info['title']}: 点击后未检测到课程页", flush=True)
        return None

    url = target.url
    platform = platform_of(url)
    try:
        if platform == "polymas":
            tasks, api_hits = await asyncio.wait_for(
                scan_polymas_tasks(target), timeout=COURSE_TIMEOUT
            )
            # 作业资料抓取: 未完成且非考试式的任务进详情页拿附件/迟交规则
            for task in tasks:
                if task.get("completed") or task.get("kind") == "exam":
                    continue
                try:
                    frag = await asyncio.wait_for(
                        scrape_task_detail(target, task, DATA_DIR),
                        timeout=DETAIL_TIMEOUT,
                    )
                    # 详情页的作业要求比列表 content 富, 优先采用
                    content_detail = frag.pop("content_detail", None)
                    task.update(frag)
                    if content_detail:
                        task["content"] = content_detail
                except asyncio.TimeoutError:
                    print(
                        f"[collector] 详情超时: {str(task.get('title') or '')[:20]}",
                        flush=True,
                    )
                except Exception as exc:
                    print(
                        f"[collector] 详情异常 {str(task.get('title') or '')[:20]}: {str(exc)[:80]}",
                        flush=True,
                    )
        elif platform == "wenda":
            tasks, api_hits = await asyncio.wait_for(
                scan_wenda_tasks(target), timeout=COURSE_TIMEOUT
            )
        else:
            tasks, api_hits = [], 0
            print(f"[collector] {card_info['title']}: 未识别平台 {urlparse(url).hostname}, 跳过", flush=True)
    except asyncio.TimeoutError:
        print(f"[collector] {card_info['title']}: 任务采集超时", flush=True)
        tasks, api_hits = [], 0
    except Exception as exc:
        print(f"[collector] {card_info['title']} 采集异常: {str(exc)[:150]}", flush=True)
        tasks, api_hits = [], 0

    # 内容上线探测(仅 polymas): 视频内容是否已发布决定 Autovisor 能否跑;
    # 顺带读「总学习进度」→ 这门课刷到百分之几(刷课目标生成器要用)。
    #
    # 2026-11-21: 探测必须**开新标签页**做。
    # 点「我的学习」会把页面导航走, 而任务采集要在**课程页**上点「我的任务」截 API
    # —— 共用页面时两者必废一个(实测: 共用页面 + 放在采任务之后, 11 门课里只有
    # 1 门探测成功, 且 progress 全是 None)。
    # 新标签页探测失败只降级探测本身(content/progress=None), 不影响作业采集。
    content_published: bool | None = None
    progress: int | None = None
    catalog: str | None = None
    if platform == "polymas":
        # 探测会偶发失手(慢网/限流时页面没渲染完 → 点不到「我的学习」),
        # 2026-11-21 连跑四轮观察到同一门课在 True/None 之间跳 —— 那会让
        # 「今晚刷哪些课」每晚不一样。重试一次, 仍不行才降级成 None。
        # 2026-11-24: 「26-27-1思想道德与法治」两次都没点着(单独手动探测却稳定
        # 读到 100%), 于是重试次数提到 3 —— 探测失败一次就等于把这门课**整个
        # 踢出自动刷课队列**, 代价比多跑一轮大。
        for attempt in range(3):
            probe = None
            try:
                probe = await portal.context.new_page()
                await probe.goto(url, wait_until="domcontentloaded", timeout=60_000)
                await probe.wait_for_timeout(4_000)
                await close_popups(probe)
                if "ai-smart-course" in (url or ""):
                    # 新版 AI 课: 页面结构不同, 走自己的探测(知识学习页的知识点
                    # 进度 + 课程视频页的视频条目数)
                    content_published, progress = await asyncio.wait_for(
                        detect_aismart_state(probe), timeout=90
                    )
                else:
                    content_published, progress = await asyncio.wait_for(
                        detect_course_state(probe), timeout=45
                    )
                # 上游 Autovisor 认课靠 DOM 选择器 —— 顺手量一下这门课它认不认得。
                # (detect_course_state 已经点过「我的学习」, 页面停在学习资源视图)
                catalog = await detect_autovisor_catalog(probe)
                if catalog is None:
                    # 新版 AI 课的课时列表在「课程视频」标签页下, 不在知识图谱页
                    catalog = await detect_catalog_via_video_tab(probe)
                print(
                    f"[collector] {card_info['title']}: 目录="
                    f"{catalog or '无(Autovisor 刷不了)'}",
                    flush=True,
                )
            except asyncio.TimeoutError:
                print(f"[collector] {card_info['title']}: 内容探测超时", flush=True)
            except Exception as exc:
                print(f"[collector] {card_info['title']} 内容探测异常: {str(exc)[:120]}", flush=True)
            finally:
                if probe is not None:
                    try:
                        await probe.close()
                    except Exception:
                        pass
            if content_published is not None:
                break
            if attempt == 0:
                print(f"[collector] {card_info['title']}: 探测没结果, 重试一次", flush=True)
                await asyncio.sleep(3)

    # 清理: 关掉新开的页面; 若课程页就是门户本体(同页跳转), 由主循环回门户
    for p in portal.context.pages:
        if p is not portal and not p.is_closed():
            try:
                await p.close()
            except Exception:
                pass

    course = {
        "title": card_info["title"],
        "badge": card_info["badge"],
        "platform": platform,
        "course_home_url": url,
        "tasks": tasks,
        "api_hits": api_hits,
        "content_published": content_published,
        # 「总学习进度」百分比(0-100); None = 读不到(页面没开/版式变了).
        # 刷课目标生成器据此判断「这门还剩多少」, 见 scripts/plan_targets.py
        "progress": progress,
        # 上游 Autovisor 认得的课程目录类型(wisdom/fusion/hike/legacy);
        # None = 上游刷不了这门课(我们的 polymas 课就是这种)。
        # 刷课目标生成器据此分流: 上游能刷的进 config.ini, 其余走自研引擎。
        "catalog": catalog,
    }
    return course


def build_report_entry(course: dict) -> dict:
    """课程采集结果 → report.json 里的课程条目.

    白名单式拷贝; 实现与踩坑记录见 scripts/course_state.py。
    """
    return course_state.build_report_entry(course)


def normalize_todo_item(item: dict) -> dict | None:
    """全局待办条目 → 统一形态; 跨平台考试条目顶层 id 为空, 取嵌套 content 里的."""
    if not isinstance(item, dict):
        return None
    content = item.get("content")
    nested = content if isinstance(content, dict) else {}
    course_id = item.get("courseId") or nested.get("spocCourseId")
    task_id = item.get("activityTaskId") or nested.get("taskId") or ""
    title = (item.get("title") or nested.get("title") or "").strip()
    if not title:
        return None
    return {
        "course_name": (item.get("courseName") or nested.get("courseName") or "").strip(),
        "activity_task_id": str(task_id),
        "course_id": course_id,
        "title": title,
        "kind": "exam" if item.get("homeworkType") == 2 else "assignment",
        # 该接口的时间是毫秒 epoch, 必须归一化成契约格式, 否则与课程流的 ISO 串
        # 同字段混用会误报「截止变更」并让下游截止/临期判定整体失效。
        "start_at": normalize_time(item.get("startTime") or nested.get("startTime")),
        "end_at": normalize_time(item.get("endTime") or nested.get("endTime")),
        "content": (nested.get("content") if nested else content) or None,
        "platform": "wenda" if item.get("courseType") == 1 else "polymas",
        "group_id": item.get("groupId"),
    }


async def scan_global_todo(page, attempts: int = 3) -> list:
    """全局待办聚合页: 只读 findNoFinishTaskTotal 响应(跨课程/跨平台 pending 视图).

    价值: 跨平台考试(如挂 wenda 课的 polymas 补考)与截止时间变更只有这里可见.
    该接口后端数据不稳定(同一会话多次观察, 考试条目时有时无), 因此多次刷新取并集,
    并用页面「待完成(N)」计数交叉核对, 集齐即提前收工.
    红线: 只读列表响应, 绝不点击任何任务/考试入口.
    """
    async def matcher(resp):
        if "findnofinishtasktotal" not in resp.url.lower():
            return None, None
        return await resp.text(), str(resp.url)

    items: list = []
    seen: set = set()
    expected: int | None = None
    for attempt in range(attempts):
        handle = CollectHandle(page, matcher)
        await handle.start()
        try:
            await page.goto(STUDY_TASK_URL, wait_until="domcontentloaded", timeout=90_000)
            await page.wait_for_timeout(7_000)
            text = await page.evaluate("document.body ? document.body.innerText : ''")
        finally:
            handle.stop()
        m = re.search(r"待完成\\((\\d+)\\)", text or "")
        if m:
            expected = max(expected or 0, int(m.group(1)))
        for cap in handle.captured:
            try:
                data = json.loads(cap["body"]).get("data") or {}
            except Exception:
                continue
            for raw in data.get("list") or []:
                item = normalize_todo_item(raw)
                if not item:
                    continue
                key = (item["course_name"], item["activity_task_id"], item["title"])
                if key in seen:
                    continue
                seen.add(key)
                items.append(item)
        if expected is not None and len(items) >= expected:
            break
    if expected is not None and len(items) < expected:
        print(
            f"[collector] 全局待办: 警告 页面显示 {expected} 项, 实抓 {len(items)} 项 "
            f"(后端不稳定仍漏抓, 建议人工核对 hike-teaching-center/stu-hike/studyTask)",
            flush=True,
        )
    return items


def merge_global_todo(report: dict, items: list) -> None:
    """把全局待办合并进报告: 已知任务刷新截止/完成态, 新任务(跨平台考试等)按课程名落位."""
    changes: list = []
    for item in items:
        course = next(
            (c for c in report["courses"] if c.get("title") == item["course_name"]),
            None,
        )
        if course is None:
            course = {
                "title": item["course_name"],
                "platform": item["platform"],
                "course_home_url": None,
                "content_published": None,
                "tasks": [],
            }
            report["courses"].append(course)
            changes.append(f"新课程条目 {item['course_name']}({item['platform']})")
        task = next(
            (t for t in course["tasks"]
             if t.get("activity_task_id") == item["activity_task_id"]
             and t.get("title") == item["title"]),
            None,
        )
        if task is None:
            course["tasks"].append({
                "activity_task_id": item["activity_task_id"],
                "title": item["title"],
                "kind": item["kind"],
                "course_id": item["course_id"],
                "group_id": item["group_id"],
                "start_at": item["start_at"],
                "end_at": item["end_at"],
                "completed": False,
                "late_submit": None,
                "total_score": None,
                "content": item["content"],
                "content_published": None,
            })
            changes.append(f"新增 {item['course_name']}｜{item['title']}")
            continue
        # 全局待办是「跨课程 pending 视图」, 粒度比逐课流粗:
        # ① 完成态以逐课流为准——平台把「等老师批阅/加分」的课堂表现类作业
        #    也留在待办里(2026-11-13 实况: 职业生涯规划 3 个课前测已交但被「打回重做」,
        #    全局待办照样列着), 无脑置 False 会让已完成作业反复复活;
        #    逐课流没给完成态(未知)时才用全局流兜底。
        # ② 截止时间只补缺失, 不覆盖逐课 ddl——全局流给的是课程级日期
        #    (课前测被从 10-31 改成 11-30 就是这么来的)。
        if task.get("completed") is None:
            task["completed"] = False
        if not task.get("end_at") and item["end_at"]:
            task["end_at"] = item["end_at"]
        elif item["end_at"] and item["end_at"] != task.get("end_at"):
            changes.append(
                f"截止不一致(以逐课为准) {item['course_name']}｜{item['title']}: "
                f"逐课 {task.get('end_at')} / 全局 {item['end_at']}"
            )
    for line in changes:
        print(f"[collector] 全局待办: {line}", flush=True)


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out", default="/app/data/report.json")
    parser.add_argument("--cookies", default="/app/data/cookies.json")
    parser.add_argument("--courses", default="", help="逗号分隔课程名; 卡片识别失败时兜底")
    args = parser.parse_args()
    manual_titles = [t.strip() for t in args.courses.split(",") if t.strip()]

    config = Config(config_path="config.ini")
    cookies = load_cookies(args.cookies)
    started = time.time()

    async with async_playwright() as pw:
        session = await create_browser_session(pw, config, cookies, logger)
        try:
            page = session.page
            login_ok = await ensure_login(session, config, args.cookies)
            report = {"generated_at": now_ts(), "login_ok": login_ok, "courses": []}
            todo_items: list = []
            if login_ok:
                try:
                    todo_items = await asyncio.wait_for(
                        scan_global_todo(page), timeout=COURSE_TIMEOUT
                    )
                    print(f"[collector] 全局待办: {len(todo_items)} 项", flush=True)
                except Exception as exc:
                    print(
                        f"[collector] 全局待办采集失败(不影响逐课): {str(exc)[:80]}",
                        flush=True,
                    )
            cards: list = []
            if not login_ok:
                print("[collector] 未登录, 只输出空报告", flush=True)
            else:
                # 门户阶段全程兜底: 任何异常降级为 0 卡片, 由「空轮询不覆盖」保住旧报告
                if await goto_portal(page):
                    await page.wait_for_timeout(3_000)
                    await close_popups(page)
                    cards = await wait_for_cards(page)
                if not cards and await goto_portal(page):
                    print("[courses] 首次未见课程卡, 点「我的课程」导航再等", flush=True)
                    await page.wait_for_timeout(3_000)
                    await close_popups(page)
                    await js_click(page, "我的课程")
                    cards = await wait_for_cards(page, attempts=10)
                if not cards and await goto_portal(page):
                    print("[courses] 仍未见课程卡, 回门户长等(慢网)", flush=True)
                    await page.wait_for_timeout(3_000)
                    await close_popups(page)
                    cards = await wait_for_cards(page, attempts=20, gap_ms=3_000)
                print(f"[courses] 门户识别 {len(cards)} 张课程卡", flush=True)
                if not cards and manual_titles:
                    cards = [{"title": t, "badge": ""} for t in manual_titles]

                if cards:
                    for index in range(len(cards)):
                        title = cards[index]["title"]
                        try:
                            course = await collect_course(page, index)
                        except Exception as exc:
                            print(f"[collector] {title} 采集异常: {str(exc)[:120]}", flush=True)
                            course = None
                        if course is None:
                            course = {
                                "title": title,
                                "badge": cards[index]["badge"],
                                "platform": "?",
                                "course_home_url": None,
                                "tasks": [],
                                "api_hits": 0,
                            }
                        entry = build_report_entry(course)
                        report["courses"].append(entry)
                        pending = sum(1 for t in course["tasks"] if not t["completed"])
                        print(
                            f"[collector] {course['title']}({course['platform']}): "
                            f"{len(course['tasks'])} 任务 ({pending} 未完成, "
                            f"api={course['api_hits']})",
                            flush=True,
                        )
                        if index < len(cards) - 1:
                            if await goto_portal(page):
                                await close_popups(page)
                                # 等卡片渲染回来, 防止下一张索引落空(慢网)
                                await wait_for_cards(page, attempts=8)

            out_path = args.out
            # 逐课采集完成后合并全局待办(补考/截止变更/新任务落位)
            merge_global_todo(report, todo_items)
            total = sum(len(c["tasks"]) for c in report["courses"])
            # 空轮询不落盘: 保留上一次的有效报告, 消费端不至于把已知 deadline 全丢掉.
            previous_exists = os.path.isfile(out_path)
            if not report["courses"] and previous_exists and not manual_titles:
                print(
                    "[collector] 本轮 0 课程, 保留既有报告不覆盖 "
                    "(如确需清空请手动删除 report.json)",
                    flush=True,
                )
            else:
                with open(out_path, "w", encoding="utf-8") as fh:
                    json.dump(report, fh, ensure_ascii=False, indent=1)
                print(f"[collector] 报告已写入 {out_path}", flush=True)
            print(
                f"[collector] 完成: {len(report['courses'])} 课程 {total} 任务 "
                f"(耗时 {time.time() - started:.0f}s)",
                flush=True,
            )
        finally:
            await session.browser.close()


if __name__ == "__main__":
    asyncio.run(main())
