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
from datetime import datetime
from urllib.parse import urlparse

sys.path.insert(0, "/app")

from modules import login as login_mod  # noqa: E402
from modules import slider as slider_mod  # noqa: E402
from modules.browser import create_browser_session  # noqa: E402
from modules.configs import Config  # noqa: E402
from modules.logger import Logger  # noqa: E402
from modules.utils import load_cookies, save_cookies  # noqa: E402
from playwright.async_api import async_playwright  # noqa: E402

logger = Logger()
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
        "start_at": start,
        "end_at": raw.get("endTime"),
        "completed": bool(raw.get("isCompleted")),
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


async def detect_content_published(page) -> bool | None:
    """点「我的学习」探测课程视频内容是否已发布(Autovisor 可跑的前提).

    None = 无法判断(页面没开/超时); False = 暂无内容; True = 出现章节/视频痕迹.
    """
    try:
        if not await js_click(page, "我的学习"):
            return None
        await page.wait_for_timeout(6_000)
        text = await asyncio.wait_for(
            page.evaluate("document.body ? document.body.innerText.slice(0, 2500) : ''"),
            timeout=STEP_TIMEOUT / 1000,
        )
    except Exception:
        return None
    # 页面有「总学习进度 0%共 N 个学习资源」字样, 以资源数为准;
    # 严禁用「学习进度」等宽松词判断——「总学习进度」恒在, 会假阳性
    count = re.search(r"共\s*(\d+)\s*个学习资源", text)
    if count:
        return int(count.group(1)) > 0
    if "暂无" in text:
        return False
    if "章节" in text or "第一章" in text or "第1章" in text:
        return True
    return None


def wenda_pick(item: dict, *keys: str):
    """防御式取字段: wenda 列表条目字段名无法预先确认, 逐个候选键试探."""
    for k in keys:
        v = item.get(k)
        if v not in (None, ""):
            return v
    return None


def wenda_item_to_contract(item: dict, kind: str) -> dict | None:
    """wenda 作业/考试条目 → 契约字段; 提取不到标题的条目放弃并留档."""
    title = wenda_pick(item, "homeworkTitle", "examTitle", "title", "name")
    if not title:
        return None
    end = wenda_pick(item, "endTime", "homeworkEndTime", "examEndTime", "endDate", "deadline")
    start = wenda_pick(item, "startTime", "homeworkStartTime", "examStartTime", "startDate")
    item_id = wenda_pick(item, "homeworkId", "examId", "id", "hwId")
    completed = wenda_pick(item, "isCompleted", "completed", "isFinish", "finished")
    if completed is None:
        status = wenda_pick(item, "status", "completeStatus", "state")
        completed = status in (1, "1", "已完成", True, "finish", "finished") if status is not None else None
    return {
        "activity_task_id": str(item_id or ""),
        "title": str(title).strip(),
        "kind": kind,
        "course_id": None,
        "group_id": None,
        "start_at": str(start) if start else None,
        "end_at": str(end) if end else None,
        "completed": bool(completed) if completed is not None else None,
        "late_submit": None,
        "score": wenda_pick(item, "score", "totalScore"),
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

    # 内容上线探测(仅 polymas): 视频内容是否发布决定 Autovisor 能否跑;
    # 失败一律降级为 None, 不影响任务采集结果
    content_published: bool | None = None
    if platform == "polymas":
        try:
            content_published = await asyncio.wait_for(
                detect_content_published(target), timeout=30
            )
        except asyncio.TimeoutError:
            print(f"[collector] {card_info['title']}: 内容探测超时", flush=True)
        except Exception as exc:
            print(f"[collector] {card_info['title']} 内容探测异常: {str(exc)[:120]}", flush=True)

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
    }
    return course


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
        "start_at": item.get("startTime") or nested.get("startTime"),
        "end_at": item.get("endTime") or nested.get("endTime"),
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
                "score": None,
                "content": item["content"],
                "content_published": None,
            })
            changes.append(f"新增 {item['course_name']}｜{item['title']}")
            continue
        # 待办流里的都是未完成; 截止有变以全局流为准
        task["completed"] = False
        if item["end_at"] and item["end_at"] != task.get("end_at"):
            changes.append(
                f"截止变更 {item['course_name']}｜{item['title']}: "
                f"{task.get('end_at')} → {item['end_at']}"
            )
            task["end_at"] = item["end_at"]
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
                        entry = {
                            "title": course["title"],
                            "platform": course["platform"],
                            "course_home_url": course["course_home_url"],
                            "tasks": course["tasks"],
                            "content_published": course.get("content_published"),
                        }
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
