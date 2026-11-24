#!/usr/bin/env python3
"""核对日报的必报条数: 新规则(同批次 ≤2 + 总量 ≤5)有没有真的生效。

起因(2026-11-24): 用户抱怨「谁让你发这么多安全漏洞的」—— 微软 11-23 一次批量发布
被 NVD 收录成 15 条高危 CVE, 必报段就发了 15 条。修好了判据(同批次扎堆抑制 +
总量上限 5 + KEV 豁免), 但**修完当天没法验证**(那天的池子已经清空, 早报也发过了),
只能等第二天早报。

这个脚本就干一件事: 早报出完后, 去 ru41 的 state.db 里找出报时那条
「安全告警播报员」任务, 数一下它实际报了几条, 然后:
  - ≤ 5 条 → 告诉用户「正常, 新规则生效了」
  - > 5 条 → 告诉用户「还是超了, 判据还有别的路径没堵上」

通知从网关容器发(bot token 只在 ru41-telegram / ru41-qq 里, worker 没有)。

跑法(生产机, 用户级 systemd 一次性触发):
    ~/.local/bin/ru41-digest-check.py
"""
from __future__ import annotations

import datetime
import json
import os
import re
import sqlite3
import subprocess
import sys

STATE_DB = "/home/windy2/ru41-mimo-smoke/data/state.db"
NOTIFY_CONTAINERS = ("ru41-telegram", "ru41-qq")
# 修复后允许的必报条数上限(与 plugins/rss/src/service.rs 的 MUST_POOL_CAP 一致)。
# ⚠️ KEV(已在野利用)是豁免的, 真的撞到 KEV 时可以超 —— 那种情况单独说一句。
EXPECTED_CAP = 5


def log(message: str) -> None:
    print(f"[digest-check] {datetime.datetime.now():%F %T} {message}", flush=True)


def notify(text: str) -> None:
    for container in NOTIFY_CONTAINERS:
        result = subprocess.run(
            ["docker", "exec", container, "ru41", "telegram", "send", "--text", text],
            capture_output=True,
            text=True,
            timeout=120,
        )
        if result.returncode == 0:
            log(f"通知已发（{container}）")
            return
        log(f"{container} 发通知失败：{((result.stdout or '') + (result.stderr or ''))[-200:]}")
    log("通知没发出去（网关容器都不可用）")


def latest_alert_task() -> tuple[int, str, int | None] | None:
    """最新一条「安全告警播报员」任务 → (id, created_at, 必报条数)。

    ⚠️ 只认**今天**的那条: 早报没出必报段时, 库里最新一条还是昨天的,
    拿它来判定会误报「还是超了」。
    """
    if not os.path.exists(STATE_DB):
        log(f"找不到 {STATE_DB}")
        return None
    try:
        con = sqlite3.connect(f"file:{STATE_DB}?mode=ro", uri=True)
        rows = con.execute(
            "select id, created_at, prompt from tasks "
            "where prompt like '%安全告警播报员%' order by id desc limit 5"
        ).fetchall()
    except Exception as exc:  # noqa: BLE001
        log(f"读 state.db 失败：{exc}")
        return None
    if not rows:
        return None

    today = datetime.datetime.now().date()
    for task_id, created_at, prompt in rows:
        stamp = parse_created_at(created_at)
        if stamp is not None and stamp.date() != today:
            continue  # 不是今天的 —— 早报今天没出必报段
        match = re.search(r"必报条目（(\d+) 条）", prompt or "")
        count = int(match.group(1)) if match else None
        return int(task_id), str(created_at), count
    return None


def parse_created_at(raw: str) -> datetime.datetime | None:
    """ru41 写的 created_at 是 UTC ISO 串(带 Z) → 换算成本地时间再取日期。

    ⚠️ 容器时区是 UTC、宿主是 CST, 不换算就会差一天(早报 08:00 出, UTC 记成 00:09)。
    """
    text = (raw or "").strip()
    if not text:
        return None
    try:
        stamp = datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        return None
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=datetime.timezone.utc)
    return stamp.astimezone()


def disable_self() -> None:
    """一次性验证: 跑完把自己停用, 别天天来烦。

    失败无害 —— 只是下次还会再核一遍(照样只发结论)。
    """
    try:
        subprocess.run(
            ["systemctl", "--user", "disable", "digest-check.timer"],
            capture_output=True,
            text=True,
            timeout=30,
        )
        log("已停用 digest-check.timer(一次性验证, 完事)")
    except Exception as exc:  # noqa: BLE001
        log(f"停用自己失败(无害): {exc}")


def main() -> int:
    dry_run = "--dry-run" in sys.argv
    code = run_check(dry_run)
    if not dry_run:
        disable_self()
    return code


def run_check(dry_run: bool) -> int:
    found = latest_alert_task()
    if found is None:
        log("今天没有必报任务 —— 可能今天没有高危条目, 或早报还没出")
        if not dry_run:
            notify(
                "ℹ️ 日报核对: 今天没有必报段\n"
                "· 没找到今天的「安全告警播报员」任务\n"
                "· 可能是今天没有高危漏洞, 也可能早报还没出(再等等)"
            )
        return 0

    task_id, created_at, count = found
    log(f"今天的必报任务 id={task_id} created={created_at} 条数={count}")

    if count is None:
        if not dry_run:
            notify(
                "ℹ️ 日报核对: 找不到必报条数\n"
                f"· 任务 id={task_id}（{created_at}）\n"
                "· 解析不出「必报条目（N 条）」—— prompt 格式变了, 值得看一眼"
            )
        return 0

    if count <= EXPECTED_CAP:
        if not dry_run:
            notify(
                "✅ 日报核对: 必报条数正常\n"
                f"· 任务 id={task_id}（{created_at}）\n"
                f"· 实际必报 {count} 条（上限 {EXPECTED_CAP}）—— 新规则生效了"
            )
        return 0

    if not dry_run:
        notify(
            "⚠️ 日报核对: 必报条数还是超了\n"
            f"· 任务 id={task_id}（{created_at}）\n"
            f"· 实际必报 {count} 条（预期 ≤ {EXPECTED_CAP}）\n"
            "· 判据还有别的路径没堵上, 需要人看一眼"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
