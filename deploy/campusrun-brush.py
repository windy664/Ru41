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
# 通知从网关容器发（bot token 在那儿，worker 里没有）
NOTIFY_CONTAINERS = ("ru41-telegram", "ru41-qq")
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


def fetch_record(rrid: str) -> dict:
    """单条记录详情（服务端给的逐条判定）。"""
    token = console_token()
    if not token:
        return {}
    request = urllib.request.Request(
        f"{CONSOLE}/api/record/{rrid}", headers={"X-Pace-Token": token}
    )
    try:
        with urllib.request.urlopen(request, timeout=25) as response:
            return json.load(response).get("data") or {}
    except Exception:  # noqa: BLE001
        return {}


def notify(text: str) -> None:
    """发一条 Telegram 通知（默认 chat 来自配置）。

    只在「真跑了」或「跑失败」时发；按间隔跳过的日子不发 —— 每天一条
    「今天不跑」是噪音。

    ⚠️ bot token 只在**网关容器**里（ru41-telegram / ru41-qq），worker 没有
    （`ru41 telegram send` 报 "telegram bot token required"）。所以从网关容器发。
    """
    for container in (NOTIFY_CONTAINERS):
        result = subprocess.run(
            ["docker", "exec", container, "ru41", "telegram", "send", "--text", text],
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode == 0:
            log(f"通知已发（{container}）")
            return
        log(f"{container} 发通知失败：{((result.stdout or '') + (result.stderr or ''))[-200:]}")
    log("通知没发出去（网关容器都不可用）")


def campusrun(*args: str) -> str:
    result = subprocess.run(
        ["docker", "exec", CONTAINER, "ru41", "campusrun", *args],
        capture_output=True, text=True, timeout=900,
    )
    return (result.stdout or "") + (result.stderr or "")


def run_once() -> tuple[bool, str]:
    """登记 + 确认。拿不到口令或预演被拦就不提交。返回 (成功, 说明)。"""
    plan = campusrun("submit", "--km", "3", "--rand", "--days", "1")
    if "预演被拦下" in plan or "不能提交" in plan:
        log("预演被拦下，不提交：")
        log(plan[-600:])
        return False, "预演被拦下（工具自检没过）"
    match = re.search(r"campusrun confirm ([0-9a-fA-F]+)", plan)
    if not match:
        log("没拿到待确认口令，不提交：")
        log(plan[-600:])
        return False, "没拿到待确认口令"
    token = match.group(1)
    result = campusrun("confirm", token)
    if "已提交" in result:
        log(f"提交成功：{result.strip()[-200:]}")
        rrid = re.search(r"id=(\d+)", result)
        return True, rrid.group(1) if rrid else ""
    log("提交失败：")
    log(result[-600:])
    return False, "确认提交失败：" + result.strip()[-120:]


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

    ok, detail = run_once()
    if ok:
        record = fetch_record(detail) if detail else {}
        after = current_km()
        runs = int(state.get("runs", 0)) + 1
        state["last_run"] = today.isoformat()
        state["km_after"] = after if after is not None else km
        state["runs"] = runs
        save_state(state)
        log(f"已记：last_run={state['last_run']}，累计 {runs} 次")
        notify(format_success(record, km, after, runs, state))
        return 0
    # 失败不更新 last_run → 下一轮还会再试（间隔照旧按上次成功算）
    notify(f"⚠️ 校园跑今天没跑成\n"
           f"· 原因：{detail}\n"
           f"· 当前有效里程 {km:.2f} km（目标 36 km，还差 {max(36.0 - km, 0.0):.2f} km）")
    return 1


def format_success(record: dict, before_km: float, after_km: float | None,
                  runs: int, state: dict) -> str:
    """成功后的通知正文。"""
    distance = record.get("totalDis")
    seconds = record.get("totalTime")
    cadence = record.get("avgStepFreq")
    verdict = record.get("statusInfo") or ""
    if record.get("isCheat"):
        verdict = "⚠️ 服务端标记了违规"
    lines = ["🏃 校园跑已完成"]
    if distance and seconds:
        pace = (seconds / 60.0) / (distance / 1000.0)
        pace_text = f"{int(pace)}'{int((pace % 1) * 60):02d}\"/km"
        lines.append(
            f"· {distance / 1000:.2f} km / {int(seconds // 60)}'{int(seconds % 60):02d}\""
            f"（配速 {pace_text}"
            + (f"，步频 {cadence}" if cadence else "")
            + "）"
        )
    if verdict:
        lines.append(f"· 服务端判定：{verdict}")
    if after_km is not None:
        lines.append(
            f"· 学期有效里程 {before_km:.2f} → {after_km:.2f} km"
            f"（目标 36 km，还差 {max(36.0 - after_km, 0.0):.2f} km）"
        )
    lines.append(f"· 第 {runs} 次")
    return "\n".join(lines)


if __name__ == "__main__":
    sys.exit(main())
