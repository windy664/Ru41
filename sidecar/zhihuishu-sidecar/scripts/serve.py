"""sidecar 服务模式: 给 ru41 调用的 HTTP 接口。

为什么要有这一层(2026-11-24):
    刷课要在**有浏览器的环境**里跑 —— 平台的完成上报是浏览器里的前端 JS 做的,
    请求体还是加密的(secretStr), 所以只能真开浏览器点。
    sidecar 镜像里有 Playwright + Xvfb; ru41 的容器两样都没有, 也没挂 docker.sock。
    所以: sidecar 常驻一个 HTTP 服务, ru41 插件通过内网调它触发刷课。
    分工还是老样子 —— **ru41 负责「什么时候刷 / 汇报 / 通知」, sidecar 负责「真的刷」**。

端点:
    GET  /health           -> {"ok": true, "busy": bool}
    GET  /status           -> 目标清单 + 上一轮结果 + 报告摘要
    POST /brush            -> 跑一轮刷课(同步, 返回摘要); 正在刷时返回 409
    POST /targets/refresh  -> 重算目标清单(跑 plan_targets.py)

红线:
    - 一次只跑一轮(进程内锁): 两轮叠着开会同时开浏览器, 把 cookies 踩坏
      (挂机中途续期、采集读到半张表, 2026-11-21 踩过)。
    - 只刷「目标清单」里的课, 不自己挑 —— 挑课是 plan_targets.py 的事。
    - 不碰小测/作业/辩论(那是刷课脚本自己的事, 这里只是调度)。

跑法(容器内, 常驻):
    DISPLAY=:99 python -u /scripts/serve.py --port 9100
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import subprocess
import sys
import threading
import time
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, "/app")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

DATA_DIR = Path(os.environ.get("SIDECAR_DATA", "/app/data"))
TARGETS_PATH = DATA_DIR / "targets.json"
BRUSH_LOG_PATH = DATA_DIR / "brush_log.json"
REPORT_PATH = DATA_DIR / "report.json"
STATE_PATH = DATA_DIR / "serve_state.json"

# 判定「今天该不该刷」统一在 sidecar 侧做, ru41 那边不自己算日期 ——
# 容器时区是 UTC, 宿主/ru41 是 CST, 两边各算各的必然错开一天。
# brush_courses 写 brush_log.generated_at 用的是容器本地时间(UTC),
# 这里解析成 UTC 再换算到 CST 比较。
try:
    from zoneinfo import ZoneInfo

    LOCAL_TZ = ZoneInfo(os.environ.get("SIDECAR_TZ", "Asia/Shanghai"))
except Exception:  # noqa: BLE001
    LOCAL_TZ = timezone(timedelta(hours=8))

# 每天几点开始刷(CST 本地时间)。之前 sidecar 的 systemd timer 是 19:00。
DEFAULT_DAILY_HOUR = int(os.environ.get("SIDECAR_DAILY_HOUR", "19"))
# 凌晨几点之前的那一轮算「昨天深夜的」，不占用当天的刷课窗口（见 due_decision）。
LATE_NIGHT_CUTOFF_HOUR = int(os.environ.get("SIDECAR_LATE_NIGHT_CUTOFF_HOUR", "6"))

# 进程内锁: 一次只跑一轮
BRUSH_LOCK = threading.Lock()


def log(message: str) -> None:
    print(f"[serve] {datetime.now():%F %T} {message}", flush=True)


def read_json(path: Path, default=None):
    try:
        with path.open(encoding="utf-8") as handle:
            return json.load(handle)
    except Exception:
        return default


def write_json(path: Path, data) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8") as handle:
            json.dump(data, handle, ensure_ascii=False, indent=1)
    except Exception as exc:  # noqa: BLE001
        log(f"写 {path} 失败: {exc}")


def course_summary() -> list[dict]:
    """目标清单里的课(挑课的逻辑在 plan_targets.py, 这里只读)。"""
    data = read_json(TARGETS_PATH, {}) or {}
    out = []
    for course in data.get("courses", []) or []:
        out.append(
            {
                "title": course.get("title"),
                "engine": course.get("engine"),
                "progress": course.get("progress"),
                "url": course.get("url"),
            }
        )
    return out


def local_now() -> datetime:
    """本地时间(CST)。容器时区是 UTC, 判断「今天」必须换算。"""
    return datetime.now(LOCAL_TZ)


def parse_generated_at(raw: str | None) -> datetime | None:
    """brush_log.generated_at → 带时区的本地时间。

    brush_courses 用 time.strftime 写, 是容器本地时间(UTC)且不带时区标记,
    所以按 UTC 解析再换算到本地。
    """
    if not raw:
        return None
    try:
        naive = datetime.strptime(raw.strip()[:19], "%Y-%m-%d %H:%M:%S")
    except Exception:
        return None
    return naive.replace(tzinfo=timezone.utc).astimezone(LOCAL_TZ)


def cst_stamp(raw: str | None) -> str | None:
    """容器本地(UTC)时间串 → 本地时间(CST)串，给 /status 暴露用。

    brush_log.generated_at 由 brush_courses 用 time.strftime 写(容器本地=UTC),
    原样透出去会被读成北京时间, 差 8 小时(2026-11-27 实测: 实际 20:49 跑完的
    那轮, /status 显示成 12:49)。

    只用于 brush_log 派生的字段; report.generated_at / targets.generated_at
    来自 collector, 已经是北京时间(见 collector.now_ts), 不能再转一次。
    """
    parsed = parse_generated_at(raw)
    return parsed.strftime("%Y-%m-%d %H:%M:%S") if parsed else raw


def due_decision(brush_log: dict) -> tuple[bool, str]:
    """今天该不该刷一轮。返回 (该刷, 理由)。

    判据:
      1. 正在刷 → 不该(上轮还没结束);
      2. 最后一轮就是今天(CST)→ 不该(一天一轮, 够了);
      3. 现在还没到每天的开工点 → 不该;
      4. 其余 → 该刷。
    ⚠️ 「凌晨那一轮」不算今天刷过(2026-11-26 实测踩到):
    上一轮跑在 23:52→00:52 跨了零点, generated_at 落在 00:52,
    于是「今天已经刷过一轮」成立 → 当天 19:00 的窗口被吃掉,
    整天不再刷。判据收紧成: 最后一轮的**时刻**落在凌晨(默认 06:00 前)
    时, 视作「昨天深夜那轮」, 不占用今天的窗口。
    """
    if BRUSH_LOCK.locked():
        return False, "上一轮还没跑完"
    now = local_now()
    last = parse_generated_at(brush_log.get("generated_at"))
    if last is not None and last.date() == now.date() and last.hour >= LATE_NIGHT_CUTOFF_HOUR:
        return False, f"今天已经刷过一轮（{last:%m-%d %H:%M}）"
    if now.hour < DEFAULT_DAILY_HOUR:
        return False, f"还没到开工点（{DEFAULT_DAILY_HOUR} 点，现在 {now:%H:%M}）"
    if last is not None:
        if last.date() == now.date():
            return True, f"凌晨 {last:%m-%d %H:%M} 那轮不算今天，今天该刷"
        return True, f"上次是 {last:%m-%d %H:%M}，今天该刷"
    return True, "还没刷过，今天该刷"


def status_payload() -> dict:
    targets = read_json(TARGETS_PATH, {}) or {}
    brush_log = read_json(BRUSH_LOG_PATH, {}) or {}
    report = read_json(REPORT_PATH, {}) or {}
    courses = report.get("courses") or []
    ready = [c.get("title") for c in courses if c.get("content_published") is True]
    due, due_reason = due_decision(brush_log)
    now = local_now()
    return {
        "ok": True,
        "busy": BRUSH_LOCK.locked(),
        "now_local": now.strftime("%Y-%m-%d %H:%M:%S"),
        "timezone": str(LOCAL_TZ),
        "due": due,
        "due_reason": due_reason,
        "daily_hour": DEFAULT_DAILY_HOUR,
        "generated_at": cst_stamp(brush_log.get("generated_at")),
        "targets_generated_at": targets.get("generated_at"),
        "courses": course_summary(),
        "last_round": {
            "generated_at": cst_stamp(brush_log.get("generated_at")),
            "results": [
                {
                    "title": r.get("title"),
                    "watched": len(r.get("watched") or []),
                    "failed": len(r.get("failed") or []),
                    "error": r.get("error"),
                }
                for r in (brush_log.get("results") or [])
            ],
        },
        "report": {
            "generated_at": report.get("generated_at"),
            "login_ok": report.get("login_ok"),
            "courses": len(courses),
            "tasks": len(report.get("tasks") or []),
            "content_ready": ready,
        },
    }


def run_brush(total_minutes: int, max_courses: int) -> dict:
    """跑一轮刷课。返回摘要。"""
    if not BRUSH_LOCK.acquire(blocking=False):
        return {"ok": False, "error": "上一轮还没跑完"}
    started = time.time()
    try:
        import brush_courses  # 延迟 import: 里面会拉起 playwright 等重依赖

        argv = ["--total-minutes", str(total_minutes)]
        if max_courses:
            argv += ["--max-courses", str(max_courses)]
        log(f"开始刷课: {argv}")
        code = asyncio.run(brush_courses.main_async(argv))
        elapsed = int(time.time() - started)
        log(f"刷课结束, 退出码 {code}, 用时 {elapsed} 秒")
        return {"ok": code == 0, "exit_code": code, "elapsed_secs": elapsed}
    except Exception as exc:  # noqa: BLE001
        log(f"刷课异常: {exc}")
        return {"ok": False, "error": str(exc)[:300]}
    finally:
        BRUSH_LOCK.release()


def refresh_targets() -> dict:
    """重算目标清单(跑 plan_targets.py)。"""
    try:
        result = subprocess.run(
            [sys.executable, "-u", "/scripts/plan_targets.py"],
            capture_output=True,
            text=True,
            timeout=600,
            cwd="/app",
        )
        log(f"plan_targets 退出码 {result.returncode}")
        tail = (result.stdout or "")[-800:]
        return {"ok": result.returncode == 0, "output": tail}
    except Exception as exc:  # noqa: BLE001
        log(f"plan_targets 异常: {exc}")
        return {"ok": False, "error": str(exc)[:300]}


class Handler(BaseHTTPRequestHandler):
    server_version = "ru41-sidecar/1"

    def _send(self, code: int, payload: dict) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802
        path = self.path.split("?")[0].rstrip("/") or "/"
        if path in ("/", "/health"):
            self._send(200, {"ok": True, "busy": BRUSH_LOCK.locked()})
        elif path == "/status":
            self._send(200, status_payload())
        else:
            self._send(404, {"ok": False, "error": f"未知路径 {path}"})

    def do_POST(self) -> None:  # noqa: N802
        path = self.path.split("?")[0].rstrip("/") or "/"
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b"{}"
        try:
            body = json.loads(raw or b"{}")
        except Exception:
            body = {}

        if path == "/brush":
            if BRUSH_LOCK.locked():
                self._send(409, {"ok": False, "error": "上一轮还没跑完"})
                return
            total_minutes = int(body.get("total_minutes") or 120)
            max_courses = int(body.get("max_courses") or 0)
            result = run_brush(total_minutes, max_courses)
            result["status"] = status_payload()
            self._send(200 if result.get("ok") else 500, result)
        elif path == "/targets/refresh":
            result = refresh_targets()
            result["status"] = status_payload()
            self._send(200 if result.get("ok") else 500, result)
        else:
            self._send(404, {"ok": False, "error": f"未知路径 {path}"})

    def log_message(self, fmt: str, *args) -> None:  # noqa: ANN002
        log("http " + (fmt % args))


def main() -> int:
    parser = argparse.ArgumentParser(description="sidecar HTTP 服务(给 ru41 调用)")
    parser.add_argument("--port", type=int, default=9100)
    parser.add_argument("--host", default="0.0.0.0")
    args = parser.parse_args()

    server = ThreadingHTTPServer((args.host, args.port), Handler)
    log(f"sidecar 服务启动: http://{args.host}:{args.port}")
    log(f"数据目录: {DATA_DIR}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        log("收到中断, 退出")
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
