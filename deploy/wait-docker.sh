#!/bin/sh
# 等 docker 守护进程就绪（最多 5 分钟）。
#
# 为什么不用 systemd 的 Requires=docker.service：
#   docker.service 是**系统级**单元，用户级 systemd 里根本不存在
#   （systemctl --user list-unit-files 里没有）。
#   写在用户级 unit 里 → 单元直接起不来：
#     Failed to start campusrun-brush.service: Unit docker.service not found. (exit=5)
#   → timer 的第一次自动触发必然失败（2026-11-23 实测）。
#
# 脚本本身只需要 docker CLI（docker inspect / docker exec），所以等守护进程就绪即可。
set -e

i=0
while [ "$i" -lt 60 ]; do
  if docker info >/dev/null 2>&1; then
    echo "docker 已就绪（等了 $((i * 5)) 秒）"
    exit 0
  fi
  i=$((i + 1))
  sleep 5
done

echo "docker 守护进程 5 分钟内没就绪" >&2
exit 1
