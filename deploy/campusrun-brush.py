#!/usr/bin/env python3
"""校园跑定时跑：每天触发一次，自己判断今天该不该跑，然后提交一条 3 公里。

为什么这么设计（2026-11-23 实测 + 反作弊维度）：

- **起跑时刻交给服务端随机**：`--rand --days 1` 让服务端在 07:00~20:00 里挑一个
  过去的时刻。写死时刻会被「起跑时刻集中度」盯上（我们 9 次全落在上午就
  会集中到 40%）。
- **间隔 >= 2 天**：连续性也是被盯的一维，我们历史最长连续也只有 2 天。
- **每次 3 公里**：单次距离过了所有见过的门槛（1.00 / 1.50 km），时长自然到
  18 分钟以上（当前任务那条「跑18分钟以上」），而 1.2 公里那条被判过
  「运动数据有问题」。
- **轨迹由 pace-forge 生成**：route 模式按瓦片路网 + **按打卡点顺序**串，
  配速/步频是 OU 过程起伏 + 起步窗口 + 弯道预判（不是匀速直线）。

目标日程（km）：

    2026-11-30  18.0   「下周之前跑完 18 公里」
    2026-12-31  36.0   学期总目标

提交成绩是**不可逆写操作**，这里自动做 submit + confirm 两步。
停：`systemctl --user disable --now campusrun-brush.timer`
"""
from __future__ import annotations

import datetime
import json
import math
import os
import re
import subprocess
import sys
import urllib.request

CONTAINER = "ru41-mimo-worker"
CONSOLE = "http://127.0.0.1:9999"
KM_PER_RUN = 3.0
# 每次多跑一点点余量：服务端算的是「有效里程」，实测会比申报值少几十米。
MIN_INTERVAL_DAYS = 2
# 目标日程：日期 → 目标公里（取第一个还没达标的目标）
SCHEDULE = [("2026-11-30", 18.0), ("2026-12-31", 36.0)]
STATE_PATH = os.path.expanduser("~/.local/state/ru41/campusrun-brush.json")


def log(message: str) -> None:
    print(f"[{datetime.datetime.now():%F %T}] {message}", flush=True)


def console_token() -> str:
    out = subprocess.run(
        ["docker", "inspect", "pace-forge", "--format",
         "{{range .Config.Env}}{{println .}}{{end}}"],
        capture_output=True, text=True, timeout=30,
    ).stdout
    for line in out.splitlines():
        if line.startswith("PACE_FORGE_TOKEN="):
            return line.split("=", 1)[1].strip()
    return ""


def current_km() -> float | None:
    """当前有效里程（km）——服务端给的 semesterValidDis，不是我们自己算的。"""
    token = console_token()
    if not token:
        log("拿不到 PACE_FORGE_TOKEN，放弃这一轮")
        return None
    request = urllib.request.Request(
        f"{CONSOLE}/api/semester", headers={"X-Pace-Token": token}
    )
    try:
        with urllib.request.urlopen(request, timeout=25) as response:
            data = json.load(response)["data"]
    except Exception as error:  # noqa: BLE001
        log(f"读学期进度失败：{error}")
        return None
    meters = data.get("summaryRaw", {}).get("semesterValidDis")
    if meters is None:
        log("响应里没有 semesterValidDis，放弃这一轮")
        return None
    return float(meters) / 1000.0


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as handle:
            data = json.load(handle)
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_state(state: dict) -> None:
    os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
    with open(STATE_PATH, "w", encoding="utf-8") as handle:
        json.dump(state, handle, ensure_ascii=False, indent=2)


def decide(km: float, last_run: str | None, today: datetime.date) -> tuple[bool, str]:
    """今天该不该跑。"""
    target: float | None = None
    deadline: datetime.date | None = None
    for raw_date, goal in SCHEDULE:
        if km < goal:
            target, deadline = goal, datetime.date.fromisoformat(raw_date)
            break
    if target is None:
        return False, f"已达标（>= {SCHEDULE[-1][1]:.0f} km），不再跑"

    need = math.ceil((target - km) / KM_PER_RUN)
    days_left = max((deadline - today).days, 1)
    interval = max(MIN_INTERVAL_DAYS, days_left // max(need, 1))

    if last_run:
        try:
            gap = (today - datetime.date.fromisoformat(last_run)).days
        except ValueError:
            gap = 999
        if gap < interval:
            return False, (
                f"距上次（{last_run}）只有 {gap} 天，间隔需 {interval} 天 —— 连续性别太密"
            )
    return True, (
        f"还差 {target - km:.2f} km（目标 {target:.0f} km，{deadline} 前），"
        f"需 {need} 次、剩 {days_left} 天 → 间隔 {interval} 天，今天跑"
    )


def campusrun(*args: str) -> str:
    result = subprocess.run(
        ["docker", "exec", CONTAINER, "ru41", "campusrun", *args],
        capture_output=True, text=True, timeout=900,
    )
    return (result.stdout or "") + (result.stderr or "")


def run_once() -> bool:
    """登记 + 确认。拿不到口令或预演被拦就不提交。"""
    plan = campusrun("submit", "--km", "3", "--rand", "--days", "1")
    if "预演被拦下" in plan or "不能提交" in plan:
        log("预演被拦下，不提交：")
        log(plan[-600:])
        return False
    match = re.search(r"campusrun confirm ([0-9a-fA-F]+)", plan)
    if not match:
        log("没拿到待确认口令，不提交：")
        log(plan[-600:])
        return False
    token = match.group(1)
    result = campusrun("confirm", token)
    if "已提交" in result:
        log(f"提交成功：{result.strip()[-200:]}")
        return True
    log("提交失败：")
    log(result[-600:])
    return False


def main() -> int:
    today = datetime.date.today()
    state = load_state()
    km = current_km()
    if km is None:
        return 1

    should_run, reason = decide(km, state.get("last_run"), today)
    log(f"当前有效里程 {km:.2f} km；{reason}")
    if not should_run:
        return 0

    if run_once():
        state["last_run"] = today.isoformat()
        state["km_after"] = km
        state["runs"] = int(state.get("runs", 0)) + 1
        save_state(state)
        log(f"已记：last_run={state['last_run']}，累计 {state['runs']} 次")
        return 0
    # 失败不更新 last_run → 下一轮还会再试（间隔照旧按上次成功算）
    return 1


if __name__ == "__main__":
    sys.exit(main())
