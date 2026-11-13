"""完成态三态化回归测试（容器内跑）: 完成态不能被"猜"成未完成.

背景(2026-11-13 生产实况):
- wenda(军事理论)作业已交且已批阅(answerScore=85, 页面标「已批阅」),
  旧映射只看 status, 批阅后 status=4 不在 (1,"1","已完成") 里 → 判成未完成,
  消费端当成「老师打回」重建待办并反复提醒, /作业 里一直挂着。
- polymas 的 activityTask/list 若某条缺 isCompleted, 旧映射默认 False,
  同样把「状态未知」当成「未完成」。

跑法: docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_completed_states.py
"""
import sys

sys.path.insert(0, "/scripts")

from collector import completed_state, wenda_completed_state  # noqa: E402

# 军事理论 第一次线上作业 的真实活样本(homeworkUser/List, 2026-11-13 抓取)
WENDA_GRADED = {
    "homeworkId": 3939045,
    "title": "第一次线上作业",
    "homeworkScore": 100.0,
    "answerScore": 85.0,
    "status": 4,
    "isShowScore": 1,
    "isComplete": "0",
    "isHomeworkRedo": 1,
    "submitDate": 1787491902000,
}
# 交了但还没批阅: status=1, 无分数
WENDA_SUBMITTED = {"homeworkId": 1, "title": "x", "status": 1}
# 没交: status=0
WENDA_UNSUBMITTED = {"homeworkId": 2, "title": "x", "status": 0}


def main() -> None:
    # wenda: 已批阅 → True
    assert wenda_completed_state(WENDA_GRADED) is True, "已批阅的作业被判成未完成"
    print("  ok  wenda 已批阅(answerScore/isShowScore) → True")

    # wenda: 已交待批阅 → True
    assert wenda_completed_state(WENDA_SUBMITTED) is True, "已提交待批阅被判成未完成"
    print("  ok  wenda 已提交待批阅(status=1) → True")

    # wenda: 未提交 → False
    assert wenda_completed_state(WENDA_UNSUBMITTED) is False, "未提交被判成已完成"
    print("  ok  wenda 未提交(status=0) → False")

    # wenda: 没有完成态线索 → None(未知), 不能猜
    assert wenda_completed_state({"homeworkId": 3, "title": "x"}) is None
    print("  ok  wenda 无完成态线索 → None(未知)")

    # wenda: isComplete="0" 不采信(已批阅的作业它也报 0)
    assert wenda_completed_state(dict(WENDA_GRADED, answerScore=None,
                                      isShowScore=None, status=None)) is None, \
        "拿 isComplete 单独当完成态了"
    print("  ok  wenda 不采信 isComplete 字段")

    # polymas: 有 isCompleted → 按值
    assert completed_state({"isCompleted": True}) is True
    assert completed_state({"isCompleted": False}) is False
    assert completed_state({"isCompleted": "1"}) is True
    print("  ok  polymas isCompleted 按值解析")

    # polymas: 缺字段 → None(未知), 不是 False
    assert completed_state({}) is None, "缺 isCompleted 被猜成未完成"
    assert completed_state({"isCompleted": None}) is None
    print("  ok  polymas 缺 isCompleted → None(未知)")

    print("全部通过")


if __name__ == "__main__":
    main()
