"""课程状态 → 报告条目: 纯函数(不依赖 playwright), 单独成模块好测.

为什么单独成文件(2026-11-21):
collector.py 顶层 import 了 playwright 和 /app/modules, 在宿主上根本 import 不了
→ 里面的纯逻辑就只能靠「上生产机手动跑」验证, 于是出了下面这个 bug 都没人发现。

踩过的坑(**别再踩**):
采集器已经算出 progress(总学习进度百分比), detect_course_state 直接调用
返回 (True, 0) 完全正常 —— 但报告里 progress 永远是 None。
根因: 主循环手工拼 report 条目时**漏了 progress 这一行**, 算出来的东西在写盘时
被丢掉, 而刷课目标生成器(plan_targets.py)读的就是它 → 选课判据整个失效。

所以约定:
1. 课程→条目的字段白名单只在 build_report_entry 一处;
2. 新增字段必须在这里显式加一行, 并在 scripts/test_report_entry.py 里加断言;
3. 这两个函数的任何改动, 回归测试都必须跑(纯 stdlib, 宿主和 CI 都能跑)。
"""
from __future__ import annotations

import re

# 「总学习进度 42%」→ 42; 只有资源数没进度时为 None.
_PROGRESS_RE = re.compile(r"总学习进度\s*(\d{1,3})\s*%")
# 「共 7 个学习资源」→ 7
_COUNT_RE = re.compile(r"共\s*(\d+)\s*个学习资源")


def parse_course_state(text: str) -> tuple[bool | None, int | None]:
    """课程页文本 → (content_published, progress_percent).

    - content_published: None = 判断不了(页面没开/版式变了);
      False = 暂无内容; True = 有章节/视频痕迹。
    - progress_percent: 「总学习进度 X%」里的 X; None = 读不到。

    ⚠️ 判断「有没有内容」只能用**资源数**, 不能用「学习进度」这类宽松词 ——
    「总学习进度」恒在(哪怕是 0%), 拿它判断内容会假阳性(2026-10 实测踩过)。
    但**读进度值本身**是另一回事: 刷课目标生成器要靠它判断「这门还剩多少」。
    """
    progress_match = _PROGRESS_RE.search(text)
    progress = int(progress_match.group(1)) if progress_match else None
    # 页面有「总学习进度 0%共 N 个学习资源」字样, 以资源数为准;
    # 严禁用「学习进度」等宽松词判断——「总学习进度」恒在, 会假阳性
    count = _COUNT_RE.search(text)
    if count:
        return int(count.group(1)) > 0, progress
    if "暂无" in text:
        return False, progress
    if "章节" in text or "第一章" in text or "第1章" in text:
        return True, progress
    return None, progress


def build_report_entry(course: dict) -> dict:
    """课程采集结果 → report.json 里的课程条目.

    ⚠️ 这是**白名单式拷贝**: 采集器算出来的新字段, 必须在这里显式加一行,
    否则会在写盘时被丢掉(2026-11-21 的 progress 丢失就是这么来的)。
    """
    return {
        "title": course["title"],
        "platform": course["platform"],
        "course_home_url": course["course_home_url"],
        "tasks": course["tasks"],
        # 视频内容是否已发布: None = 判断不了, False = 暂无内容, True = 有内容
        "content_published": course.get("content_published"),
        # 「总学习进度」百分比(0-100); None = 读不到(页面没开/版式变了)
        "progress": course.get("progress"),
    }
