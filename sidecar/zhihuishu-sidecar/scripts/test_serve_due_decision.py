"""容器内自测: serve 的「今天该不该刷」判定(含凌晨跨零点那轮).

背景(2026-11-26 实测): 上一轮刷课跑在 23:52→00:52 跨了零点, generated_at
落在 00:52, 于是「今天已经刷过一轮」成立 → 当天 19:00 的窗口被吃掉, 整天不再刷。
判据收紧成: 最后一轮的**时刻**落在凌晨(默认 06:00 前)时, 视作「昨天深夜那轮」,
不占用今天的窗口。

注意 brush_log.generated_at 是**容器本地时间(UTC)**, 下面的串都按 UTC 写;
判定内部会换算回北京时间再比日期/时刻。

跑法(在装有 sidecar 镜像的机器上):
    docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_serve_due_decision.py
"""
import os
import sys
from datetime import datetime

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import serve  # noqa: E402


def case(name: str, now_cst: datetime, brush_log_utc: str, want_due: bool):
    """把 serve 的「现在」钉死, 让判定可复现。"""
    serve.local_now = lambda: now_cst
    due, reason = serve.due_decision({"generated_at": brush_log_utc})
    assert due == want_due, f"{name}: due={due} 期望 {want_due}（理由: {reason}）"
    print(f"  ok  {name} → due={due}（{reason}）")


NOW = datetime(2026, 11, 27, 19, 0)

# 1) 上一轮跑在今天凌晨 00:52(跨零点那轮, UTC 是前一天 16:52)
#    → 视作「昨天深夜那轮」, 不占用今天的窗口, 该刷。
case("今天凌晨 00:52 那轮 + 今天 19:00", NOW, "2026-11-26 16:52:00", True)

# 2) 上一轮是昨天 15:10 → 今天还没刷, 该刷。
case("昨天 15:10 那轮 + 今天 19:00", NOW, "2026-11-26 07:10:00", True)

# 3) 上一轮是今天 15:10 → 今天已经刷过, 不该再刷。
case("今天 15:10 那轮 + 今天 19:00", NOW, "2026-11-27 07:10:00", False)

# 4) 还没到每天的开工点(19:00) → 不该刷。
case("今天 15:10 那轮 + 今天 10:00", datetime(2026, 11, 27, 10, 0), "2026-11-27 07:10:00", False)

# 5) 一次都没刷过 → 该刷。
case("从没刷过 + 今天 19:00", NOW, "", True)

print("全部通过")
