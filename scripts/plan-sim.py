#!/usr/bin/env python3
"""长期规划模拟：算课表空闲 → 评估固定槽位稳定性 → 推荐节奏 → 铺排里程碑。

用途：验证 docs/longterm-planning.md 的方案在真实课表上跑不跑得通。
不写任何数据，纯读（sqlite 只读打开），可以在生产机上直接跑。

    python3 scripts/plan-sim.py --db ~/ru41-mimo-smoke/data/state.db
    python3 scripts/plan-sim.py --db ... --target 2026-11-19 --end 2027-01-03

输出三段：
  1. 槽位稳定性表（每个候选槽位在学期内可用几个块）
  2. 目标函数推荐的两个固定槽位（容量 + 间隔）
  3. 块级排期：哪一天学什么（内容含 Rust Book 章节、练习、验收产出）
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

# 学期末的验收目标（对应块级内容）
ACCEPTANCE = [
    ("能读懂并修复借用检查器的三类报错", "块4 产出"),
    ("能用 Result + ? 组合错误流，且错误信息对用户可读", "块6 产出"),
    ("能用 trait 抽象行为、用泛型约束类型", "块9 产出"),
    ("能判断何时需要手写生命周期标注", "块10 产出"),
    ("能从零写出一个带测试的 CLI 工具", "块12 产出"),
]


# 块级学习内容：与 docs/plans/rust-baseline/ 下的示例文件一一对应。
# 每条 = (标题, Rust Book 章节, 动手练习, 验收产出)
CURRICULUM = [
    ("块1 环境与基本语法", "Ch1 + Ch3.1-3.5",
     "cargo new / cargo test 跑通；shadowing、if-match、loop 返回值",
     "能独立建项目并让测试跑起来", "block01_basics.rs"),
    ("块2 所有权（上）", "Ch4.1-4.2",
     "移动语义三个规则；不可变/可变借用；借用不夺所有权",
     "能解释 'cannot move out of borrowed content' 为什么错", "block02_ownership.rs"),
    ("块3 切片 + 结构体 + 方法", "Ch4.3 + Ch5",
     "切片借用不复制；struct/impl/method；UTF-8 边界",
     "写出 Rectangle + area()，理解字符串是字节不是字符", "block03_structs.rs"),
    ("块4 借用检查器实操", "Ch4.2 三类错误",
     "借用后修改 / 借用后移动 / 借用后销毁；三种修法",
     "能读懂并修好 5 个借用错误", "block04_borrow_checker.rs"),
    ("块5 枚举与模式匹配", "Ch6",
     "enum 携带数据；Option；match 穷尽；if let；let-else；@ 绑定",
     "用 Option 建模「可能没有值」，不用空指针", "block05_enums.rs"),
    ("块6 错误处理", "Ch9",
     "Result；? 运算符；From 转换；自定义错误 + Display/Error trait",
     "写一条 ? 串联的错误流，且错误信息可读", "block06_errors.rs"),
    ("块7 模块与包", "Ch7",
     "mod/use/pub；可见性层级；嵌套模块；crate:: 路径",
     "把代码拆成模块树并控制可见性", "block07_modules.rs"),
    ("块8 常用集合", "Ch8",
     "Vec/String/HashMap；entry API 计数惯用法",
     "写一个词频统计小工具", "block08_collections.rs"),
    ("块9 泛型与 Trait", "Ch10.1-10.2",
     "泛型函数 + trait bound；trait 默认实现；impl Trait",
     "定义一个 trait 并用泛型函数约束它", "block09_traits.rs"),
    ("块10 生命周期", "Ch10.3",
     "省略规则；<'a> 标注；结构体持有引用；生命周期约束",
     "能判断什么时候该写标注、什么时候编译器能推出来", "block10_lifetimes.rs"),
    ("块11 动手：猜数字游戏", "Ch2 全流程",
     "输入解析与容错；比较逻辑抽枚举；随机源做成依赖以便测试",
     "一个能玩的游戏 + 5 个通过的测试", "block11_guessing_game.rs"),
    ("块12 动手：CLI 搜索工具", "Ch12",
     "读参数/读文件；逻辑抽成可测函数；错误走 stderr；退出码",
     "能跑的 minigrep + 3 个通过的测试", "block12_minigrep.rs"),
    ("块13 机动：补漏 + 阶段复盘", "—",
     "补前面欠的块；把 12 个示例串成一篇笔记；列出还不会的点",
     "能口头讲清所有权/借用/生命周期三者的关系", None),
]

# 示例目录（用于校验每个块都有对应且能编译的文件）
EXAMPLE_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "docs", "plans", "rust-baseline")


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

    def free_segments(self, day, day_start=8 * 60, day_end=23 * 60, lunch=(12 * 60, 14 * 60)):
        """当天所有空闲段（扣掉课程与午休）。"""
        occupied = sorted(self.busy.get(day, []))
        segments = []
        cursor = day_start
        for (s, e, _) in occupied:
            if s > cursor:
                segments.append((cursor, s))
            cursor = max(cursor, e)
        if cursor < day_end:
            segments.append((cursor, day_end))
        out = []
        for (a, b) in segments:
            if a < lunch[1] and b > lunch[0]:
                if a < lunch[0]:
                    out.append((a, lunch[0]))
                if b > lunch[1]:
                    out.append((lunch[1], b))
            else:
                out.append((a, b))
        return [(a, b) for a, b in out if b > a]


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

    # 碎片池：取第一个完整周（周一~周日），统计 10-29 分钟的空隙
    week_start = today + dt.timedelta(days=(7 - today.weekday()) % 7 or 7)
    if week_start > end:
        week_start = today
    week_days = [week_start + dt.timedelta(days=i) for i in range(7)]
    print(f"\n【1.5】碎片池（10-29 分钟的空隙）｜样本周 {week_start} ~ {week_start + dt.timedelta(days=6)}")
    print(f"{'日期':<12}{'星期':<6}{'碎片':<26}{'整块(>=30min)'}")
    print("-" * 100)
    frag_total = 0
    frag_count = 0
    for d in week_days:
        frags, blocks = [], []
        for (a, b) in tt.free_segments(d):
            length = b - a
            if 10 <= length < 30:
                frags.append((a, b))
            elif length >= 30:
                blocks.append((a, b))
        frag_total += sum(b - a for a, b in frags)
        frag_count += len(frags)
        fs = " ".join(f"{a//60:02d}:{a%60:02d}-{b//60:02d}:{b%60:02d}" for a, b in frags) or "—"
        bs = " ".join(f"{a//60:02d}:{a%60:02d}-{b//60:02d}:{b%60:02d}" for a, b in blocks) or "—"
        print(f"{d.isoformat():<12}周{'一二三四五六日'[d.weekday()]:<5} {fs:<26} {bs}")
    print("-" * 100)
    print(f"碎片池合计 {frag_total} 分钟（{frag_total/60:.1f}h），{frag_count} 段，平均 {frag_total/max(frag_count,1):.0f} 分钟/段")
    print(f"→ 「每天 20 分钟 × 5 天」需要 100 分钟：{'装得下' if frag_total >= 100 else '装不下'}")
    print(f"→ 「每天 15 分钟 × 5 天」需要 75 分钟：{'装得下' if frag_total >= 75 else '装不下'}")

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
    print(f"\n★ 推荐：{a[2]} + {b[2]}　{len(slots)} 块 / {capacity:.0f}h　间隔 {best[2]} 天")

    print("\n【3】块级排期（每块 2h；内容见 docs/plans/rust-baseline.md）")
    need_h = len(CURRICULUM) * BLOCK / 60
    print(f"    需要 {len(CURRICULUM)} 块 = {need_h:.0f}h；学期内容量 {capacity:.0f}h → "
          f"{'排得下，余 %.0fh' % (capacity - need_h) if capacity >= need_h else '排不下，缺 %.0fh' % (need_h - capacity)}")

    for i, d in enumerate(slots):
        if i >= len(CURRICULUM):
            break
        title, chapter, drill, deliver, example = CURRICULUM[i]
        wd = "一二三四五六日"[d.weekday()]
        print(f"\n    {d}（第{tt.week_of(d)}周 周{wd}）{title}")
        print(f"        读：Rust Book {chapter}")
        print(f"        练：{drill}")
        print(f"        验收：{deliver}")
        if example:
            path = os.path.join(EXAMPLE_DIR, example)
            mark = "✓ 存在" if os.path.exists(path) else "✗ 缺失"
            print(f"        示例：docs/plans/rust-baseline/{example}  {mark}")

    if len(slots) < len(CURRICULUM):
        print(f"\n    ⚠ 学期内只排得下 {len(slots)} 块，还差 {len(CURRICULUM) - len(slots)} 块（见寒假规划）")
    else:
        print(f"\n    ✓ 学期内 {len(CURRICULUM)} 块全部排完（{slots[0]} → {slots[len(CURRICULUM)-1]}）")

    print("\n【4】学期末验收标准")
    for (goal, where) in ACCEPTANCE:
        print(f"    - {goal}   ← {where}")
    con.close()


if __name__ == "__main__":
    main()
