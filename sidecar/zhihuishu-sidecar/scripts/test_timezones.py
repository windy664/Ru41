"""容器内自测: sidecar 对外暴露的时间戳统一是北京时间(UTC+8).

背景: sidecar 容器没设 TZ(playwright 基础镜像是 UTC), `datetime.now()` 与
`time.strftime` 拿到的都是 UTC. 两处踩过:
  1. collector.now_ts() 用 datetime.now() 写 report.generated_at, 而同一份报告里
     平台时间经 normalize_time 归一化成北京时间——一份文件两种时区, 读的人差
     8 小时(2026-11-27 实测: 04:37 写出的报告, generated_at 却是
     "2026-11-26 20:32:14").
  2. serve.py 把 brush_log.generated_at(容器本地=UTC)原样透出, /status 里
     「上一轮: 2026-11-26 12:49:00」实际是 20:49 跑的.

跑法(在装有 sidecar 镜像的机器上):
    docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_timezones.py
"""
import os
import sys
import time
from datetime import datetime, timedelta, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from collector import CST, normalize_time, now_ts  # noqa: E402


def shift(stamp: str, seconds: int) -> str:
    return (datetime.strptime(stamp, "%Y-%m-%d %H:%M:%S")
            + timedelta(seconds=seconds)).strftime("%Y-%m-%d %H:%M:%S")


# 1) now_ts() 必须是北京时间(与容器本地时区无关).
want = datetime.now(CST).strftime("%Y-%m-%d %H:%M:%S")
got = now_ts()
assert got in (want, shift(want, 1), shift(want, -1)), \
    f"now_ts()={got!r}, 期望北京时间 {want!r}"
print(f"  ok  now_ts() -> {got!r}（北京时间）")

# 2) 同一份报告里的两个时间戳必须同一口钟: now_ts() 与 epoch 归一化结果.
epoch_stamp = normalize_time(int(time.time()))
assert epoch_stamp in (got, shift(got, 1), shift(got, -1)), (epoch_stamp, got)
print(f"  ok  normalize_time(epoch) -> {epoch_stamp!r} 与 now_ts() 同钟")

# 3) serve 暴露 brush_log(容器本地=UTC)时必须 +8 小时.
#    这条属于 serve 侧, 单独放 test_serve_status_stamp.py——一个文件一条判据,
#    免得一个测试因为另一个模块缺失而「因错误原因失败」。
print("全部通过")
