#!/usr/bin/env python3
"""长期规划模拟：算课表空闲 → 评估固定槽位稳定性 → 推荐节奏 → 铺排里程碑。

用途：验证 docs/longterm-planning.md 的方案在真实课表上跑不跑得通。
不写任何数据，纯读（sqlite 只读打开），可以在生产机上直接跑。

    python3 scripts/plan-sim.py --db ~/ru41-mimo-smoke/data/state.db
    python3 scripts/plan-sim.py --db ... --target 2026-11-19 --end 2027-01-03

输出三段：
  1. 槽位稳定性表（每个候选槽位在学期内可用几个块）
  2. 目标函数推荐的两个固定槽位（容量 + 间隔）
  3. 里程碑铺排结果，以及学期内容量 vs 目标需求的对比
"""

import argparse
import datetime as dt
import json
import os
import sqlite3
from collections import defaultdict

OFFSET = 8 * 3600  # 东八区；Ru41 的 TodoOptions.offset_seconds 默认值
BLOCK = 120        # 固定块时长（分钟）

# 候选槽位池：(星期几, 起始分钟, 名称)。星期几 1=周一 … 7=周日
POOL = [
    (1, 19 * 60, "周一晚"), (2, 19 * 60, "周二晚"), (3, 19 * 60, "周三晚"),
    (4, 19 * 60, "周四晚"), (5, 19 * 60, "周五晚"),
    (6, 10 * 60, "周六上午"), (6, 14 * 60, "周六下午"),
    (7, 10 * 60, "周日上午"), (7, 14 * 60, "周日下午"),
    (2, 9 * 60, "周二上午"), (4, 9 * 60, "周四上午"),
]

# 示例目标：Rust 基础（分钟）
MILESTONES = [
    ("M1 环境与基本语法", 240),
    ("M2 所有权与借用", 360),
    ("M3 类型系统 enum/match/Option/Result", 240),
    ("M4 泛型/trait/生命周期入门", 240),
    ("M5 动手：写一个 CLI 小工具", 360),
]


def hhmm_to_min(text):
    h, m = text.split(":")
    return int(h) * 60 + int(m)


class Timetable:
    """从 records 里读课表快照与作息元数据，展开成每日占用。"""

    def __init__(self, con):
        cur = con.cursor()
        snapshot = None
        for (body,) in cur.execute("SELECT body FROM records WHERE kind='jwxt_timetable'"):
            obj = json.loads(body)
            # 挑 meetings 非空的那条（库里通常有一条旧学期的空快照）
            if obj.get("meetings"):
                snapshot = obj
        if snapshot is None:
            raise SystemExit("没有非空的 jwxt_timetable 快照")
        self.snapshot = snapshot
        self.meta = json.loads(
            cur.execute("SELECT body FROM records WHERE kind='jwxt_meta' LIMIT 1").fetchone()[0]
        )
        self.times = [hhmm_to_min(t) for t in self.meta["section_times"]]
        self.duration = self.meta["class_duration_mins"]
        self.semester_start = dt.date.fromisoformat(self.meta["semester_start"])
        self.busy = defaultdict(list)

    def week_of(self, day):
        return (day - self.semester_start).days // 7 + 1

    def load_events(self, con, start, end):
        """用户自建事件（非 jwxt 课表快照），避免与课表重复计算。"""
        for (body,) in con.execute("SELECT body FROM records WHERE kind='event'"):
            obj = json.loads(body)
            if (obj.get("source") or {}).get("channel") == "jwxt":
                continue
            if obj.get("recurrence") not in (None, "none"):
                continue
            begin = dt.datetime.fromtimestamp(obj["starts_at"] + OFFSET, dt.timezone.utc)
            end_ = dt.datetime.fromtimestamp(obj["ends_at"] + OFFSET, dt.timezone.utc)
            day = begin.date()
            if not (start <= day <= end):
                continue
            s = begin.hour * 60 + begin.minute
            e = end_.hour * 60 + end_.minute
            if e <= s:
                e = 24 * 60
            self.busy[day].append((s, e, "【自建】" + obj["title"][:22]))

    def expand_courses(self, start, end):
        for m in self.snapshot["meetings"]:
            ws, we = m.get("start_section"), m.get("end_section")
            if not ws or not we:
                continue
            s = self.times[ws - 1]
            e = self.times[we - 1] + self.duration
            for week in m["weeks"]:
                day = self.semester_start + dt.timedelta(
                    days=(week - 1) * 7 + (m["weekday"] - 1)
                )
                if start <= day <= end:
                    self.busy[day].append((s, e, f"{m['course']}@{m['classroom']}"))

    def conflict(self, day, s, e):
        return [(bs, be, t) for bs, be, t in self.busy.get(day, []) if bs < e and be > s]


def dates_on(today, end, weekday):
    out = []
    for i in range((end - today).days + 1):
        day = today + dt.timedelta(days=i)
        if day.weekday() + 1 == weekday:
            out.append(day)
    return out


def free_dates(tt, today, end, weekday, start_min):
    return [d for d in dates_on(today, end, weekday) if not tt.conflict(d, start_min, start_min + BLOCK)]


def gap_days(a, b):
    """两个槽位在日历上的间隔天数（0 = 连着两天）。"""
    if a == b:
        return 7
    return (b - a) % 7


def recommend(tt, today, end):
    """目标函数：容量优先，其次间隔，间隔=0（连着两天）扣分。"""
    cands = []
    for (wd1, s1, n1) in POOL:
        ok1 = free_dates(tt, today, end, wd1, s1)
        if not ok1:
            continue
        for (wd2, s2, n2) in POOL:
            if (wd2, s2) <= (wd1, s1) or wd1 == wd2:
                continue
            ok2 = free_dates(tt, today, end, wd2, s2)
            if not ok2:
                continue
            cap = len(ok1) + len(ok2)
            gap = gap_days(wd1, wd2)
            score = cap * 2 + (gap * 3 if gap >= 2 else 0) - (6 if gap == 0 else 0)
            cands.append((score, cap, gap, (wd1, s1, n1), (wd2, s2, n2), len(ok1), len(ok2)))
    cands.sort(key=lambda x: -x[0])
    return cands


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default=os.path.expanduser("~/ru41-mimo-smoke/data/state.db"))
    ap.add_argument("--target", default="2026-11-19")
    ap.add_argument("--end", default="2027-01-03")
    args = ap.parse_args()

    today = dt.date.fromisoformat(args.target)
    end = dt.date.fromisoformat(args.end)

    con = sqlite3.connect(f"file:{args.db}?mode=ro", uri=True)
    tt = Timetable(con)
    tt.expand_courses(today, end)
    tt.load_events(con, today, end)

    print("=" * 100)
    print(f"长期规划模拟｜今天 {today} → 学期末 {end}｜第 {tt.week_of(today)}-{tt.week_of(end)} 周")
    print(f"课表快照 {len(tt.snapshot['meetings'])} 门课｜学期起始 {tt.semester_start}")
    print("=" * 100)

    print("\n【1】槽位稳定性（学期内可用块数）")
    print(f"{'槽位':<10}{'块数':<8}{'可用':<8}{'冲突':<8}{'首个冲突'}")
    print("-" * 100)
    for (wd, s, name) in POOL:
        all_dates = dates_on(today, end, wd)
        ok = free_dates(tt, today, end, wd, s)
        first = "—"
        for d in all_dates:
            c = tt.conflict(d, s, s + BLOCK)
            if c:
                first = f"{d} {c[0][2][:22]}"
                break
        print(f"{name:<10}{len(all_dates):<8}{len(ok):<8}{len(all_dates)-len(ok):<8}{first}")

    cands = recommend(tt, today, end)
    print("\n【2】固定槽位推荐（容量 + 间隔）")
    print(f"{'组合':<22}{'容量h':<8}{'间隔天':<8}{'得分':<8}{'明细'}")
    print("-" * 100)
    for (score, cap, gap, a, b, na, nb) in cands[:8]:
        print(f"{a[2]+' + '+b[2]:<22}{cap*2:<8}{gap:<8}{score:<8}{a[2]}({na}) + {b[2]}({nb})")

    best = cands[0]
    a, b = best[3], best[4]
    slots = sorted(set(free_dates(tt, today, end, a[0], a[1]) + free_dates(tt, today, end, b[0], b[1])))
    capacity = len(slots) * BLOCK / 60
    need = sum(m[1] for m in MILESTONES) / 60
    print(f"\n★ 推荐：{a[2]} + {b[2]}　{len(slots)} 块 / {capacity:.0f}h　间隔 {best[2]} 天")

    print("\n【3】里程碑铺排")
    for t, m in MILESTONES:
        print(f"    - {t:<36} {m//60}h")
    print(f"    合计 {need:.0f}h；学期内容量 {capacity:.0f}h → "
          f"{'排得下，余 %.0fh' % (capacity - need) if capacity >= need else '排不下，缺 %.0fh' % (need - capacity)}")

    remaining = {t: m for t, m in MILESTONES}
    for d in slots:
        if not remaining:
            break
        current = next((t for t, m in remaining.items() if m > 0), None)
        if current is None:
            break
        used = min(BLOCK, remaining[current])
        print(f"    {d}（{tt.week_of(d)}周 {'一二三四五六日'[d.weekday()]}） → {current}（{used//60}h）")
        remaining[current] -= used
        if remaining[current] <= 0:
            del remaining[current]
    print(f"    学期内学不完：{list(remaining.keys()) or '无'}")
    con.close()


if __name__ == "__main__":
    main()
