"""容器内自测: 平台时间归一化(毫秒 epoch → 契约 ISO 串).

背景: 智慧树「全局待办」接口(findNoFinishTaskTotal)的 endTime/startTime 返回毫秒
epoch(如 "1794628800000"), 课程流返回的是平台格式化好的 ISO 串——两种形态不同源.
不归一化会误报「截止变更」, 并让下游 parse_platform_time 解析失败, 截止/临期判定
与待办 due_at 一起失效.

跑法(在装有 sidecar 镜像的机器上):
    docker run --rm -v "$PWD/scripts:/scripts:ro" zhihuishu-sidecar:local \
        python -u /scripts/test_normalize_time.py
"""
import sys

sys.path.insert(0, "/scripts")

from collector import normalize_time, normalize_todo_item  # noqa: E402

# 1794628800000 ms == 2026-11-14 12:00:00 北京时间(UTC+8).
# 注意: sidecar 容器没设 TZ(playwright 基础镜像是 UTC), 这条断言同时验证换算
# 不依赖容器本地时区——用 datetime.fromtimestamp 不带 tz 会差 8 小时.
BEIJING_NOON = "2026-11-14 12:00:00"

cases = [
    ("1794628800000", BEIJING_NOON),          # 毫秒 epoch 串
    (1794628800000, BEIJING_NOON),            # 毫秒 epoch 数字
    ("1794628800", BEIJING_NOON),              # 秒 epoch 串
    ("2026-11-14 12:00:00", "2026-11-14 12:00:00"),  # ISO 串原样
    ("2026-11-14 12:00", "2026-11-14 12:00"),        # 缺秒原样
    ("20261114", "20261114"),                 # 紧凑日期不当 epoch
    ("未知", "未知"),                          # 认不出来原样透传
]
for raw, want in cases:
    got = normalize_time(raw)
    assert got == want, f"normalize_time({raw!r}) = {got!r}, 期望 {want!r}"
    print(f"  ok  normalize_time({raw!r}) -> {got!r}")

assert normalize_time(None) is None
assert normalize_time("") is None
print("  ok  None / 空串 -> None")

# 全局待办条目: 毫秒 endTime 必须落成契约格式.
item = {
    "courseName": "军事理论",
    "title": "第一次线上作业",
    "activityTaskId": "T-1",
    "endTime": "1794628800000",
}
out = normalize_todo_item(item)
assert out is not None
assert out["end_at"] == BEIJING_NOON, out
print(f"  ok  normalize_todo_item end_at -> {out['end_at']!r}")

# 课程流已经是 ISO 串时不得被改写(否则会凭空造出「截止变更」).
iso_item = dict(item, endTime="2026-11-14 12:00:00")
out_iso = normalize_todo_item(iso_item)
assert out_iso is not None
assert out_iso["end_at"] == "2026-11-14 12:00:00", out_iso
print(f"  ok  normalize_todo_item 保留 ISO 串 -> {out_iso['end_at']!r}")

print("全部通过")
