"""容器内自测: /status 暴露的 brush_log 时间戳是北京时间.

背景(2026-11-27 实测): brush_courses 用 time.strftime 写 brush_log.generated_at,
那是**容器本地时间(UTC)**. serve.py 原样透出, 于是 /status 里
「上一轮: 2026-11-26 12:49:00」实际是 20:49 跑完的那轮——差 8 小时.
边界上统一转成北京时间.

注意 report.generated_at / targets.generated_at 来自 collector, 已经是北京时间
(见 collector.now_ts), 不能再转一次——这条由 test_timezones.py 守。

跑法(在装有 sidecar 镜像的机器上):
    docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_serve_status_stamp.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from serve import cst_stamp  # noqa: E402

# 容器本地(UTC) 12:49 == 北京时间 20:49。
utc_stamp = "2026-11-26 12:49:00"
got = cst_stamp(utc_stamp)
assert got == "2026-11-26 20:49:00", f"cst_stamp({utc_stamp!r}) = {got!r}"
print(f"  ok  cst_stamp({utc_stamp!r}) -> {got!r}")

# 跨零点的也要对：UTC 2026-11-26 16:52 == 北京时间 2026-11-27 00:52。
got = cst_stamp("2026-11-26 16:52:00")
assert got == "2026-11-27 00:52:00", got
print(f"  ok  cst_stamp('2026-11-26 16:52:00') -> {got!r}")

# 空值/脏值不许炸, 原样透传。
assert cst_stamp(None) is None
assert cst_stamp("") == ""
assert cst_stamp("不是时间") == "不是时间"
print("  ok  cst_stamp 空值/脏值原样透传")

print("全部通过")
