"""回归测试: 报告条目不能丢字段 + 课程状态解析.

为什么这个文件存在(2026-11-21):
采集器算出了 progress(总学习进度百分比), detect_course_state 直接调用返回
(True, 0) 完全正常, 但报告里 progress **永远是 None** —— 主循环拼 report 条目时
漏了 progress 这一行, 算出来的东西在写盘时被丢掉。刷课目标生成器
(plan_targets.py)读的就是它 → 「这门还剩多少」的判据整个失效。

这类 bug 光靠「跑一遍看看」抓不住(字段少了不报错), 所以固定成断言:
**报告条目里出现的每个字段, 都必须能从采集结果里拿到值。**

跑法(纯 stdlib, 宿主/CI 都能跑, 不需要 playwright):
    python3 -u scripts/test_report_entry.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from course_state import build_report_entry, parse_course_state  # noqa: E402

FAILED: list[str] = []


def check(cond: bool, label: str) -> None:
    if cond:
        print(f"  ok   {label}")
    else:
        print(f"  FAIL {label}")
        FAILED.append(label)


# ---- 1. 报告条目字段不许丢 --------------------------------------------------

COURSE = {
    "title": "国家安全教育",
    "platform": "polymas",
    "course_home_url": "https://hike-teaching-center.polymas.com/x/stu/course-home",
    "tasks": [{"title": "作业一", "completed": False}],
    "content_published": True,
    "progress": 0,
    # 这些是采集器内部字段, **不该**进报告条目
    "badge": "",
    "api_hits": 3,
}

entry = build_report_entry(COURSE)
check(entry.get("progress") == 0, "progress 被带进报告条目")
check(entry.get("content_published") is True, "content_published 被带进报告条目")
check(entry.get("title") == "国家安全教育", "title 被带进报告条目")
check(entry.get("platform") == "polymas", "platform 被带进报告条目")
check(entry.get("course_home_url", "").endswith("course-home"), "course_home_url 被带进报告条目")
check(entry.get("tasks") == COURSE["tasks"], "tasks 被带进报告条目")
check("badge" not in entry, "内部字段 badge 不进报告条目")
check("api_hits" not in entry, "内部字段 api_hits 不进报告条目")

# progress 读不到时必须是 None 而不是缺键/报错
entry_none = build_report_entry(
    {
        "title": "程序设计基础",
        "platform": "polymas",
        "course_home_url": "https://x/1",
        "tasks": [],
        "content_published": None,
        "progress": None,
    }
)
check("progress" in entry_none, "progress 为 None 时仍有该键")
check(entry_none["progress"] is None, "progress 为 None 时值为 None")
check(entry_none["content_published"] is None, "content_published 为 None 时值为 None")

# 采集彻底失败时的兜底课程(没有 content_published/progress 键)也不能炸
entry_broken = build_report_entry(
    {
        "title": "军事理论",
        "platform": "?",
        "course_home_url": None,
        "tasks": [],
    }
)
check(entry_broken["progress"] is None, "兜底课程 progress 为 None")
check(entry_broken["content_published"] is None, "兜底课程 content_published 为 None")


# ---- 2. 课程页文本解析 ------------------------------------------------------

# 真实页面文本(2026-11-21 探针抓的): 「总学习进度 0%共 5 个学习资源」
c, p = parse_course_state("总学习进度 0%共 5 个学习资源")
check(c is True and p == 0, f"「总学习进度 0%共 5 个学习资源」→ (True, 0), 实得 {(c, p)}")

c, p = parse_course_state("总学习进度 42%共 7 个学习资源")
check(c is True and p == 42, f"「总学习进度 42%共 7 个学习资源」→ (True, 42), 实得 {(c, p)}")

# 只有资源数、没有进度 → 内容有、进度读不到
c, p = parse_course_state("学习资源\n共 3 个学习资源")
check(c is True and p is None, f"只有资源数 → (True, None), 实得 {(c, p)}")

# 资源数为 0 → 还没放内容
c, p = parse_course_state("总学习进度 0%共 0 个学习资源")
check(c is False, f"「共 0 个学习资源」→ content False, 实得 {c}")

# 暂无内容
c, p = parse_course_state("暂无学习内容")
check(c is False, f"「暂无」→ content False, 实得 {c}")

# 只有章节字样 → 有内容
c, p = parse_course_state("第一章 绪论")
check(c is True and p is None, f"「第一章」→ (True, None), 实得 {(c, p)}")

# 判不出来
c, p = parse_course_state("加载中")
check(c is None and p is None, f"无关文本 → (None, None), 实得 {(c, p)}")

# ⚠️ 「总学习进度」恒在, 不能拿它判断「有没有内容」(2026-10 实测踩过的假阳性)
c, p = parse_course_state("总学习进度 0%")
check(c is None, f"只有「总学习进度」不判内容(防假阳性), 实得 {c}")


# ---- 汇总 -------------------------------------------------------------------

if FAILED:
    print(f"\n✗ {len(FAILED)} 项断言失败:")
    for item in FAILED:
        print(f"  - {item}")
    sys.exit(1)
print("\n✓ 全部通过")
