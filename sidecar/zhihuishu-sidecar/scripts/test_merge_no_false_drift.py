"""容器内回归测试: 全局待办合并不得误报「截止变更」.

场景(2026-11-13 生产实况): 智慧树「全局待办」接口(findNoFinishTaskTotal)的
endTime 返回毫秒 epoch("1794628800000"), 课程流返回的是平台格式化好的 ISO 串
("2026-11-14 12:00:00")——两者是同一时刻, 但写法不同.

归一化缺失时, merge_global_todo 会把二者当成"截止时间变了", 每轮采集都往日志里
打一条「截止变更」, Ru41 侧还会因此推送一张「截止变更」卡片(用户实况: 📚 作业提醒(1)
🕐 军事理论｜第一次线上作业｜截止变更：2026-11-14 12:00 → 1794628800000), 且
parse_platform_time 解析失败会让该作业的截止/临期判定与待办 due_at 一起失效.

跑法(在装有 sidecar 镜像的机器上):
    docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_merge_no_false_drift.py
"""
import contextlib
import io
import sys

sys.path.insert(0, "/scripts")

from collector import merge_global_todo, normalize_todo_item  # noqa: E402

# 课程流(逐课采集)给出的任务: 平台已格式化的 ISO 串
COURSE_TASK = {
    "activity_task_id": "3939045",
    "title": "第一次线上作业",
    "kind": "assignment",
    "course_id": "x",
    "group_id": None,
    "start_at": None,
    "end_at": "2026-11-14 12:00:00",
    "completed": False,
    "late_submit": None,
    "score": None,
    "content": None,
    "content_published": None,
}

# 全局待办流(findNoFinishTaskTotal)的原始条目: endTime 是毫秒 epoch
RAW_TODO_ITEM = {
    "courseName": "军事理论",
    "activityTaskId": "3939045",
    "spocCourseId": "x",
    "title": "第一次线上作业",
    "homeworkType": 1,
    "startTime": None,
    "endTime": "1794628800000",
    "content": {},
    "courseType": 0,
    "groupId": None,
}


def make_report() -> dict:
    return {
        "generated_at": "2026-11-13 09:42:29",
        "login_ok": True,
        "courses": [
            {
                "title": "军事理论",
                "platform": "polymas",
                "course_home_url": None,
                "content_published": None,
                "tasks": [dict(COURSE_TASK)],
            }
        ],
    }


def run_merge(report: dict, raw_item: dict) -> tuple[str, str]:
    """走生产同款路径: 平台原始条目 → normalize_todo_item → merge_global_todo."""
    item = normalize_todo_item(raw_item)
    assert item is not None
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        merge_global_todo(report, [item])
    return buf.getvalue().strip(), report["courses"][0]["tasks"][0]["end_at"]


def main() -> None:
    # 归一化: 毫秒 epoch 必须落成与课程流同格式的串
    item = normalize_todo_item(RAW_TODO_ITEM)
    assert item is not None
    assert item["end_at"] == "2026-11-14 12:00:00", item
    print(f"  ok  normalize_todo_item end_at -> {item['end_at']!r}")

    # 同一时刻的两种写法合并后不该产生任何变更
    log, end_at = run_merge(make_report(), RAW_TODO_ITEM)
    assert "截止变更" not in log, f"误报截止变更: {log}"
    assert log == "", f"合并不该有额外日志: {log}"
    assert end_at == "2026-11-14 12:00:00", end_at
    print(f"  ok  同一时刻不同写法 → 无变更日志, end_at={end_at!r}")

    # 真的改了截止时间(ISO → ISO)仍要报出来, 别把真变更一起吞掉
    changed = dict(RAW_TODO_ITEM, endTime="2026-11-20 23:59:00")
    log, end_at = run_merge(make_report(), changed)
    assert "截止变更" in log, f"真变更被漏报了: {log}"
    assert end_at == "2026-11-20 23:59:00", end_at
    print(f"  ok  真变更仍被报出: {log}")

    print("全部通过")


if __name__ == "__main__":
    main()
