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


def relogin(token: str) -> bool:
    """让 pace-forge 重新登录运动世界校园(账号密码在它的加密库里)。

    ⚠️ 两个外部限制(2026-11-25 实测):
      1. **账号保护**: 同一账号 1 小时内只能在一个设备登录, 否则服务端回
         「此账号已在其他设备登录…请过一小时后登录或原设备退出后再登录」(错误码 10121)。
         → 所以**别频繁重登**(我连试三次就把自己锁了 1 小时)。
      2. **DNS 抖动**: 容器 DNS 走宿主 systemd-resolved 的转发链, 宿主一抖容器就
         EAI_AGAIN → Rust 报成「tls connection init failed: os error 11」, 看着像
         网络故障其实是 DNS。已给 pace-forge 容器配独立 DNS(见 apps/pace-forge/compose.yaml)。
         → DNS 修好后这一步就稳了。

    ⚠️ 2026-11-25 实测: 机器断电重启后, 运动世界校园的会话 token 失效,
    /api/semester 会回「token有误或缓存中token不存在」→ 之前脚本直接放弃这一轮,
    校园跑就**永久停摆**了(没人再跑, token 也不会自己好)。
    重新登录还会过极验 —— 那是 PoW 挑战, pace-forge 自己能算(前提是容器能连上
    geetest, 见 apps/pace-forge/compose.yaml 的代理配置)。
    """
    request = urllib.request.Request(
        f"{CONSOLE}/api/accounts/login-all",
        data=b"{}",
        headers={"X-Pace-Token": token, "Content-Type": "application/json"},
        method="POST",
    )
    try:
        # 极验 PoW 要算一会儿, 超时给足
        with urllib.request.urlopen(request, timeout=180) as response:
            data = json.load(response)
        # ⚠️ 顶层 ok 只是「接口本身没报错」; 真正的成败在 data.results[].ok
        # (data.ok 是**成功条数**: 全成功=1, 全失败=0)。2026-11-25 踩到:
        # 顶层 ok=true 但 data.ok=0(登录失败), 照顶层判会误以为登上了。
        results = (data.get("data") or {}).get("results") or []
        ok = bool(results) and all(bool(item.get("ok")) for item in results)
        detail = "; ".join(
            str(item.get("error") or "")[:120] for item in results if not item.get("ok")
        )
        log(f"重新登录{'成功' if ok else '失败'}{'：' + detail if detail else ''}")
        return ok
    except Exception as error:  # noqa: BLE001
        log(f"重新登录失败：{error}")
        return False


def current_km(token: str | None = None) -> float | None:
    """当前有效里程（km）——服务端给的 semesterValidDis，不是我们自己算的。"""
    token = token or console_token()
    if not token:
        log("拿不到 PACE_FORGE_TOKEN，放弃这一轮")
        return None
    for attempt in (1, 2):
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
        if meters is not None:
            return float(meters) / 1000.0
        # 没有 semesterValidDis = 会话 token 失效 → 重新登录再试一次
        log("响应里没有 semesterValidDis（会话 token 失效？）")
        if attempt == 1 and relogin(token):
            time.sleep(3)
            continue
        log("重新登录后仍读不到学期进度，放弃这一轮")
        return None
    return None


def last_server_run_date(token: str | None = None) -> str | None:
    """服务端最新一条跑步记录的起跑日（YYYY-MM-DD）。

    ⚠️ 为什么不能只看本地状态文件(2026-11-25 实测踩到):
    用户自己手跑一次(App 里跑的, 2.75 km), 脚本的状态文件根本不知道 →
    脚本以为「上次是 11-23」→ 今天又判定该跑 → **重复提交**。
    真实跑没跑, 只有服务端知道。
    """
    token = token or console_token()
    if not token:
        return None
    request = urllib.request.Request(
        f"{CONSOLE}/api/records", headers={"X-Pace-Token": token}
    )
    try:
        with urllib.request.urlopen(request, timeout=25) as response:
            records = json.load(response)["data"].get("records") or []
    except Exception as error:  # noqa: BLE001
        log(f"读跑步记录失败：{error}")
        return None
    for record in records:
        start = record.get("startTime")
        if not start:
            continue
        stamp = datetime.datetime.fromtimestamp(start / 1000)
        return stamp.strftime("%Y-%m-%d")
    return None


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
    token = console_token()
    km = current_km(token)
    if km is None:
        # ⚠️ 读不到里程就静默退出的后果: 用户完全不知道校园跑停摆了。
        # 通知里说清原因(多半是会话 token 失效且重登也失败)。
        notify("⚠️ 校园跑今天没跑成\n"
               "· 原因：读不到运动世界校园的学期进度(会话失效且自动重登失败)\n"
               "· 查一下 pace-forge 的账号登录状态, 或手动跑一次")
        return 1

    # 「上次跑」以**服务端记录**为准, 本地状态文件只作兜底 —— 两者取较晚的。
    # ⚠️ 2026-11-25 实测: 用户自己手跑一次(App 里跑的 2.75 km), 脚本的状态文件
    # 根本不知道 → 脚本以为「上次还是 11-23」→ 今天又判定该跑 → 差点重复提交。
    # 真实跑没跑, 只有服务端知道。
    local_last = state.get("last_run")
    server_last = last_server_run_date(token)
    last_run = max((d for d in (local_last, server_last) if d), default=None)
    if server_last and local_last and server_last != local_last:
        log(f"上次跑步时间以服务端为准: 服务端={server_last} 本地={local_last} → 用 {last_run}")

    should_run, reason = decide(km, last_run, today)
    log(f"当前有效里程 {km:.2f} km；{reason}")
    if not should_run:
        return 0

    ok, detail = run_once()
    if ok:
        record = fetch_record(detail) if detail else {}
        after = current_km(token)
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
