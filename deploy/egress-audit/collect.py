#!/usr/bin/env python3
"""mihomo 出站审计采集（P3+ observe-only）：增量拉 mihomo 日志，过滤容器网段
（172.16/12 = docker），归因容器名后转 JSONL 落 ru41 数据目录 /egress/。
cron 每分钟拉起，flock 防并发；state 存最后一条日志的 docker 时间戳游标。"""
import json, os, re, subprocess, sys
from datetime import datetime, timezone

STATE = "/home/windy2/egress-audit/state"
OUT_DIR = "/home/windy2/ru41-mimo-smoke/.ru41/egress"
KEEP_DAYS = 30
SRC_RE = re.compile(r"^172\.(1[6-9]|2\d|3[01])\.")
# info 行：[TCP] src --> dst match RULE using PROXY；warning 行（拨号失败）同源同目标。
LINE_RE = re.compile(
    r'^(\S+) \S+ msg="\[(TCP|UDP)\] .*?(\d+\.\d+\.\d+\.\d+):(\d+) --> (\S+?)(?: error:| match (.*?) using (.*?))?"'
)

def cursor():
    try:
        return open(STATE).read().strip()
    except OSError:
        return None

def container_map():
    mapping = {}
    try:
        out = subprocess.run(
            ["docker", "network", "inspect", "ru41-net", "bridge"],
            capture_output=True, text=True, timeout=20,
        ).stdout
        for net in json.loads(out or "[]"):
            for cid, conf in (net.get("Containers") or {}).items():
                ip = conf.get("IPv4Address", "").split("/")[0]
                if ip:
                    mapping[ip] = net["Name"] + ":" + conf.get("Name", cid[:12])
    except Exception as e:
        print(f"container map failed: {e}", file=sys.stderr)
    return mapping

def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    last = cursor()
    cmd = ["docker", "logs", "--timestamps"]
    if last:
        cmd += ["--since", last]
    else:
        cmd += ["--since", "2m"]
    cmd.append("mihomo")
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=50)
    lines = (proc.stdout + proc.stderr).splitlines()
    if not lines:
        return
    cmap = container_map()
    latest = last
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    out_path = os.path.join(OUT_DIR, f"conns-{today}.jsonl")
    count = 0
    with open(out_path, "a") as f:
        for line in lines:
            ts, _, rest = line.partition(" ")
            if latest and ts <= latest:
                continue
            latest = ts
            m = LINE_RE.match(rest)
            if not m:
                continue
            src_ip = m.group(3)
            if not SRC_RE.match(src_ip):
                continue
            rec = {
                "ts": ts,
                "net": m.group(2),
                "container": cmap.get(src_ip),
                "src": f"{src_ip}:{m.group(4)}",
                "dst": m.group(5),
                "rule": m.group(6) or "",
                "proxy": m.group(7) or "",
            }
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            count += 1
    # 轮换清理：顺手删 30 天前的旧文件。
    for name in os.listdir(OUT_DIR):
        path = os.path.join(OUT_DIR, name)
        if os.path.getmtime(path) < (os.path.getmtime(out_path) - KEEP_DAYS * 86400):
            os.remove(path)
    with open(STATE, "w") as f:
        f.write(latest or "")
    if count:
        print(f"{count} conns appended to {out_path}")

if __name__ == "__main__":
    main()
