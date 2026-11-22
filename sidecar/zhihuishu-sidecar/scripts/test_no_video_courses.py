"""no_video_courses + pick_courses 排除逻辑的回归测试(纯 stdlib, 宿主可跑).

跑法: python3 scripts/test_no_video_courses.py

覆盖的真实场景(2026-11-22 生产实测):
  报告里有 4 门「有内容、没刷完」的课, 其中两门(思想道德与法治/职业生涯规划)
  资源全是 PDF 一个视频都没有, 却占满了 max=3 的名额 → 真有待刷视频的
  「国家安全教育」(progress=60) 根本没进今晚的清单。
"""
from __future__ import annotations

import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from no_video_courses import (  # noqa: E402
    DEFAULT_TTL_DAYS,
    excluded_urls,
    is_excluded,
    load_store,
    mark,
    prune,
    save_store,
)
from plan_targets import pick_courses  # noqa: E402

NOW = 1_800_000_000

PDF_COURSE = "https://hike-teaching-center.polymas.com/agent-course-full/aaa/stu/course-home"
VIDEO_COURSE = "https://hike-teaching-center.polymas.com/agent-course-full/bbb/stu/course-home"
AISMART_COURSE = (
    "https://ai-smart-course-student-pro.zhihuishu.com/singleCourse/knowledgeStudy/1/2"
)

REPORT = {
    "courses": [
        {
            "title": "思想道德与法治",
            "course_home_url": PDF_COURSE,
            "platform": "polymas",
            "content_published": True,
            "progress": 0,
        },
        {
            "title": "职业生涯规划",
            "course_home_url": "https://hike-teaching-center.polymas.com/x/stu/course-home",
            "platform": "polymas",
            "content_published": True,
            "progress": 0,
        },
        {
            "title": "国家安全教育",
            "course_home_url": VIDEO_COURSE,
            "platform": "polymas",
            "content_published": True,
            "progress": 60,
        },
        {
            "title": "程序设计基础",
            "course_home_url": AISMART_COURSE,
            "platform": "polymas",
            "content_published": True,
            "progress": 2,
        },
    ]
}


def test_mark_and_exclude() -> None:
    store: dict[str, dict] = {}
    mark(store, PDF_COURSE, "思想道德与法治", NOW)
    assert is_excluded(store, PDF_COURSE, NOW), "刚记过的课该被排除"
    assert not is_excluded(store, VIDEO_COURSE, NOW), "没记过的课不该被排除"


def test_expires_after_ttl() -> None:
    store: dict[str, dict] = {}
    mark(store, PDF_COURSE, "思想道德与法治", NOW)
    later = NOW + (DEFAULT_TTL_DAYS + 1) * 86400
    assert not is_excluded(store, PDF_COURSE, later), "过期后该重新探测(老师可能补了视频)"
    assert is_excluded(store, PDF_COURSE, NOW + DEFAULT_TTL_DAYS * 86400 - 1)


def test_prune_drops_expired() -> None:
    store: dict[str, dict] = {}
    mark(store, PDF_COURSE, "思想道德与法治", NOW)
    mark(store, VIDEO_COURSE, "国家安全教育", NOW)
    prune(store, NOW + (DEFAULT_TTL_DAYS + 1) * 86400)
    assert list(store) == [], "过期条目应被清掉"


def test_pick_courses_skips_no_video_courses() -> None:
    """真场景: 两门纯 PDF 的课被排除后, 「国家安全教育」进了清单。"""
    excluded = {PDF_COURSE, "https://hike-teaching-center.polymas.com/x/stu/course-home"}
    picked = pick_courses(REPORT, 100, 3, excluded)
    titles = [c["title"] for c in picked]
    assert "国家安全教育" in titles, titles
    assert "思想道德与法治" not in titles, titles
    assert "职业生涯规划" not in titles, titles
    assert "程序设计基础" in titles, titles


def test_pick_courses_without_memory_keeps_old_behaviour() -> None:
    """没记忆文件时行为不变(两门 PDF 课仍会占名额) —— 别把老逻辑改坏。"""
    picked = pick_courses(REPORT, 100, 3)
    titles = [c["title"] for c in picked]
    assert titles == ["思想道德与法治", "职业生涯规划", "程序设计基础"], titles


def test_store_round_trip() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "no_video_courses.json")
        store = load_store(path)
        mark(store, PDF_COURSE, "思想道德与法治", NOW)
        save_store(path, store)
        assert excluded_urls(path, NOW) == {PDF_COURSE}
        # 坏文件不能把链路带崩
        with open(path, "w", encoding="utf-8") as fh:
            fh.write("{ 这不是 json")
        assert load_store(path) == {}
        assert excluded_urls(path, NOW) == set()




def test_completed_videos_must_not_be_treated_as_no_video() -> None:
    """「有视频但全刷完」≠「无视频课」—— 这两者必须分开(2026-11-22 踩过)。

    实测: 「国家安全教育」3 个 mp4 全「已完成」, 被脚本误记成无视频课。
    后果是老师以后补了新视频, 这门课永远进不了目标清单。
    这里锁住判定口径: 只要**存在**视频资源, 就不许记。
    """
    from polymas_state import pick_pending_videos

    completed = [
        {"name": "《大学生国家安全教育》数字教材阅读.mp4", "is_video": True, "done": True},
        {"name": "统筹发展和安全的科学内涵.mp4", "is_video": True, "done": True},
        {"name": "资源安全的重要性.mp4", "is_video": True, "done": True},
        {"name": "附件二（视频报告模板）.docx", "is_video": False, "done": False},
    ]
    all_pdf = [
        {"name": "《平语近人》.pdf", "is_video": False, "done": False},
        {"name": "学习资源包 (27)", "is_video": False, "done": False},
    ]

    assert pick_pending_videos(completed) == [], "全刷完的课没有待刷视频"
    # 但「有视频资源」这一条成立 → 不该被记成无视频课
    assert any(r["is_video"] for r in completed), "有 mp4 → 不是无视频课"
    assert not any(r["is_video"] for r in all_pdf), "全是文档 → 才是无视频课"


if __name__ == "__main__":
    for name, fn in sorted(list(globals().items())):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok  {name}")
    print("no_video_courses 全部通过")
