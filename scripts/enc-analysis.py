#!/usr/bin/env python3
"""enc 样本分析（科研线 C）：读 enc-samples.jsonl，找结构。

用法：
    python3 scripts/enc-analysis.py <enc-samples.jsonl> [更多.jsonl ...]

分析内容：
- 总量 / 按活动 / 按来源分布
- 同窗口多样本的字符级差异（判定位随时间变化的区段）
- base36/base32/hex 解码尝试：找嵌入的 epoch 时间戳
- 长度与字符集分布
退出码：无样本 0，有样本 1（方便脚本串联）。"""

import base64
import json
import sys
import time
from pathlib import Path


def load(paths):
    samples = []
    for p in paths:
        for line in Path(p).read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                samples.append(json.loads(line))
            except json.JSONDecodeError:
                print(f"[warn] 非法行跳过: {line[:80]}")
    samples.sort(key=lambda s: s.get("ts", 0))
    return samples


def charset_of(enc: str) -> str:
    if enc.isdigit():
        return "digits"
    if all(c in "0123456789ABCDEF" for c in enc):
        return "hex-ish"
    if enc.isupper():
        return "upper-alnum"
    if enc.islower():
        return "lower-alnum"
    return "mixed"


def try_embedded_epoch(enc: str) -> list:
    """对样本做常见编码解码，找形如 epoch 秒（2020-2035）的值。"""
    found = []
    # base32（学习通系编码常用 RFC4648 无填充大写）
    for pad in range(4):
        try:
            raw = base64.b32decode(enc + "=" * pad)
            for w in (4, 8):
                if len(raw) >= w:
                    for off in range(0, min(len(raw) - w + 1, 4)):
                        v = int.from_bytes(raw[off : off + w], "big")
                        if 1577836800 <= v <= 2051222400:  # 2020-2035
                            found.append(("base32", off, w, v))
        except Exception:
            break
    # base36
    try:
        v = int(enc, 36)
        if 1577836800 <= v <= 2051222400:
            found.append(("base36", 0, len(enc), v))
        # 高位截断也试
        v2 = v // (36**6)
        if 1577836800 <= v2 <= 2051222400:
            found.append(("base36>>6chars", 0, len(enc) - 6, v2))
    except Exception:
        pass
    return found


def diff_positions(a: str, b: str) -> list:
    if len(a) != len(b):
        return [-1]
    return [i for i, (x, y) in enumerate(zip(a, b)) if x != y]


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    samples = load(sys.argv[1:])
    if not samples:
        print("无样本。等真实 QR 窗口攒数据。")
        return 0

    print(f"样本总数: {len(samples)}")
    by_activity = {}
    for s in samples:
        by_activity.setdefault(s["activity_id"], []).append(s)
    print(f"活动数: {len(by_activity)}")
    for aid, group in sorted(by_activity.items()):
        print(f"\n=== 活动 {aid}（{len(group)} 样本）===")
        for s in group:
            when = time.strftime("%m-%d %H:%M:%S", time.localtime(s.get("ts", 0)))
            mark = "✓" if s.get("ok") else "✗"
            print(
                f"  {when} [{s.get('source')}] {mark} enc={s.get('enc')} "
                f"resp={s.get('response', '')[:40]}"
            )

    encs = [s["enc"] for s in samples if s.get("enc")]
    if encs:
        lengths = sorted({len(e) for e in encs})
        charsets = sorted({charset_of(e) for e in encs})
        print(f"\n长度分布: {lengths}")
        print(f"字符集: {charsets}")

    # 同活动多样本 → 差异位分析（判断旋转的是全量还是部分）
    for aid, group in sorted(by_activity.items()):
        ok_encs = [s for s in group if s.get("source") == "probe"] or group
        for i in range(len(ok_encs) - 1):
            a, b = ok_encs[i]["enc"], ok_encs[i + 1]["enc"]
            if len(a) == len(b):
                d = diff_positions(a, b)
                print(f"\n活动 {aid} 相邻样本差异位: {d}")
                print(f"  t0={ok_encs[i]['ts']} t1={ok_encs[i+1]['ts']} Δt={ok_encs[i+1]['ts']-ok_encs[i]['ts']}s")

    print("\n=== 嵌入时间戳探测 ===")
    for s in samples:
        hits = try_embedded_epoch(s["enc"])
        if hits:
            for kind, off, w, v in hits:
                print(
                    f"  活动 {s['activity_id']} enc 含疑似 epoch（{kind} off={off} w={w}）: "
                    f"{v} = {time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(v))} "
                    f"（样本 ts 偏差 {s['ts'] - v}s）"
                )
    print("\n（无输出 = 未发现嵌入时间戳结构；样本不足时此结论无意义）")
    return 1


if __name__ == "__main__":
    sys.exit(main())
