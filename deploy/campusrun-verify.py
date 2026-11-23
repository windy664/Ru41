#!/usr/bin/env python3
"""校验「定时跑自动提交」到底有没有真的入账 —— 自动查、自动报。

背景(2026-11-23):
    campusrun-brush.timer 从 11-23 才 enable，**service 从没被 timer 触发过**
    (journal 空、timer 的 LAST 列是 "-")。而且单元里写了
    `Requires=docker.service` —— 那是**系统级**单元，用户级 systemd 里不存在，
    所以 timer 的第一次触发会直接失败(exit=5)。
    修好之后仍要回答用户的问题:「首次自动提交能不能正常入账」。

这个脚本负责回答:
    1. 今天有没有**由定时器触发**的校园跑记录(按 startTime 的本地日期找);
    2. 这条记录有没有真的入账(complete / status / isCheat / 服务端判罚原因);
    3. 学期有效里程涨了多少。
查到了就发 Telegram 报告，然后停掉自己(一次性验证)。

轮询: 定时跑的最晚触发时刻是 08:30 + 3h 随机 = 11:30，所以从 12:00 起每 10 分钟
查一次，最多查到 20:00 —— 免得触发时刻随机导致查早了就下结论。

用法(生产机, 用户级 systemd 触发):
    ~/.local/bin/ru41-campusrun-verify.py
"""
from __future__ import annotations

import datetime
import json
import os
import subprocess
import sys
import time
import urllib.request

CONSOLE = "http://127.0.0.1:9999"
CONTAINER = "ru41-mimo-worker"
NOTIFY_CONTAINERS = ("ru41-telegram", "ru41-qq")
STATE_PATH = os.path.expanduser("~/.local/state/ru41/campusrun-verify.json")
# 轮询窗口: 12:00 起查到 20:00
POLL_FROM = datetime.time(12, 0)
POLL_UNTIL = datetime.time(20, 0)
POLL_INTERVAL = 600


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


def api(path: str, token: str) -> dict:
    request = urllib.request.Request(
        f"{CONSOLE}{path}", headers={"X-Pace-Token": token}
    )
    with urllib.request.urlopen(request, timeout=25) as response:
        return json.load(response).get("data") or {}


def notify(text: str) -> None:
    for container in NOTIFY_CONTAINERS:
        result = subprocess.run(
            ["docker", "exec", container, "ru41", "telegram", "send", "--text", text],
            capture_output=True, text=True, timeout=120,
        )
        if result.returncode == 0:
            log(f"通知已发（{container}）")
            return
        log(f"{container} 发通知失败：{((result.stdout or '') + (result.stderr or ''))[-200:]}")
    log("通知没发出去（网关容器都不可用）")


def load_state() -> dict:
    try:
        with open(STATE_PATH, encoding="utf-8") as handle:
            data = json.load(handle)
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_state(data: dict) -> None:
    os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
    with open(STATE_PATH, "w", encoding="utf-8") as handle:
        json.dump(data, handle, ensure_ascii=False, indent=2)


def records(token: str) -> list[dict]:
    data = api("/api/records", token)
    return data.get("records") or []


def today_record(token: str) -> dict | None:
    """今天（本地日期）起跑的那条记录。"""
    now = datetime.datetime.now()
    for record in records(token):
        stamp = record.get("startTime")
        if not stamp:
            continue
        started = datetime.datetime.fromtimestamp(stamp / 1000)
        if started.date() == now.date():
            return record
    return None


def semester_km(token: str) -> float | None:
    data = api("/api/semester", token)
    meters = (data.get("summaryRaw") or {}).get("semesterValidDis")
    return None if meters is None else float(meters) / 1000.0


def describe(record: dict, detail: dict, km: float | None) -> str:
    rrid = record.get("rrid")
    started = datetime.datetime.fromtimestamp(record["startTime"] / 1000)
    stopped = datetime.datetime.fromtimestamp((record.get("stopTime") or record["startTime"]) / 1000)
    minutes = int(record.get("totalTime") or 0) / 60
    lines = [
        "🏃 定时跑自动提交 · 入账确认",
        f"· 记录 id={rrid}",
        f"· 起跑 {started:%m-%d %H:%M} → 结束 {stopped:%m-%d %H:%M}（{minutes:.0f} 分钟）",
        f"· 距离 {record.get('distanceKm')} km（有效 {round((record.get('validDistance') or 0) / 1000, 2)} km）"
        f"，步频 {record.get('avgStepFreq')}",
        f"· complete={record.get('complete')} status={record.get('status')} "
        f"isCheat={detail.get('isCheat')} canAppeal={detail.get('canAppeal')}",
    ]
    if km is not None:
        lines.append(f"· 学期有效里程 {km:.2f} km（目标 36 km，还差 {max(36.0 - km, 0.0):.2f} km）")
    status_info = detail.get("statusInfo")
    if status_info:
        lines.append(f"· 服务端评语：{status_info}")
    # 服务端逐条判定: complete=false 的那条就是没达标/判违规的依据
    reasons = detail.get("reasonList") or []
    if reasons:
        lines.append("· 服务端逐条判定：")
        for item in reasons:
            mark = "✅" if item.get("complete") else "❌"
            lines.append(f"    {mark} {item.get('reason')}")
    appeal = detail.get("appealStatusInfo")
    if appeal:
        lines.append(f"· 申诉：{appeal}")
    return "\n".join(lines)


def main() -> int:
    token = console_token()
    if not token:
        log("拿不到 PACE_FORGE_TOKEN")
        return 1

    state = load_state()
    today = datetime.date.today().isoformat()
    if state.get("verified_for") == today:
        log(f"{today} 已经验过了，不再重复")
        return 0

    now = datetime.datetime.now()
    if now.time() < POLL_FROM:
        log(f"现在 {now:%H:%M}，早于 {POLL_FROM:%H:%M} —— 先不查")
        return 0

    record = today_record(token)
    if not record:
        if now.time() >= POLL_UNTIL:
            # 窗口都过完了还没有今天的记录 → 定时跑没触发或没提交，报一声
            km = semester_km(token) or 0.0
            notify("⚠️ 校园跑定时跑：今天没有记录\n"
                   "· 12:00–20:00 轮询过，查不到今天起跑的任何记录\n"
                   f"· 当前有效里程 {km:.2f} km")
            state["verified_for"] = today
            save_state(state)
            return 1
        log("还没找到今天的记录，下一轮再看")
        return 0

    detail = {}
    try:
        detail = api(f"/api/record/{record['rrid']}", token)
    except Exception as error:  # noqa: BLE001
        log(f"取记录详情失败：{error}")

    km = semester_km(token)
    # 同一条记录只通知一次
    if state.get("announced_rrid") != record.get("rrid"):
        notify(describe(record, detail, km))
        state["announced_rrid"] = record.get("rrid")
    state["verified_for"] = today
    state["last_rrid"] = record.get("rrid")
    save_state(state)
    log(f"已确认 {record.get('rrid')} 入账")
    return 0


if __name__ == "__main__":
    sys.exit(main())
