#!/usr/bin/env bash
# 起/重建 campusrun 的常驻容器（只读轮询 + 事后复查，**不提交任何跑步**）。
#
# 背景（2026-11-26）：
# - 每个插件一个容器，容器内跑 `ru41 schedule run <模块>`（见 ru41-course-brush 等）。
# - campusrun 之前**没有**自己的容器 ⇒ 它的 tracker 从没跑过：
#   「每小时拉一次学期进度」与「提交后第 3 天自动复查有没有被回扫判违规」都落不了地。
# - 注意：campusrun 的 tracker 是**只读**的（拉学期进度/记录/违规状态），
#   提交要用户明确下命令才会走 ⇒ 起这个容器不会自动刷跑、不会动你的成绩。
#
# 用法：bash deploy/start-campusrun.sh
set -euo pipefail

ROOT="${RU41_HOME:-$HOME/ru41-mimo-smoke}"
IMAGE="${RU41_IMAGE:-ru41:local}"
NAME="ru41-campusrun"

docker rm -f "$NAME" >/dev/null 2>&1 || true

# 令牌从**现有容器**直接取（真值只在主机内存里过一手，不落盘、不进 git、不进日志）。
# 取不到再退到 pace-forge 的 compose（那里也有同一个值）。
TOKEN="$(docker inspect ru41-qq --format '{{range .Config.Env}}{{println .}}{{end}}' 2>/dev/null \
  | sed -n 's/^PACE_FORGE_TOKEN=//p' | head -1)"
if [ -z "$TOKEN" ]; then
  TOKEN="$(sed -n 's/^ *PACE_FORGE_TOKEN: *"\(.*\)" *$/\1/p' \
    "$HOME/apps/pace-forge/compose.yaml" 2>/dev/null | head -1)"
fi
if [ -z "$TOKEN" ]; then
  echo "[campusrun] 拿不到 PACE_FORGE_TOKEN —— 复查会一直失败（看 pace-forge/compose.yaml）" >&2
  exit 1
fi

docker run -d --name "$NAME" \
  --network ru41-net \
  --restart unless-stopped \
  -e TZ=Asia/Shanghai \
  -e HOME=/home/node \
  -e CODEX_HOME=/data/codex \
  -e PACE_FORGE_TOKEN="$TOKEN" \
  -v "$ROOT/plugins.d:/etc/ru41/plugins.d:ro" \
  -v "$ROOT/ru41.toml:/etc/ru41/ru41.toml:ro" \
  -v "$ROOT/skills:/usr/share/ru41/plugins:ro" \
  -v "$ROOT/plugins:/etc/ru41/plugins:ro" \
  -v "$ROOT/data:/data" \
  -v "$HOME/obsidian-vault:/vault" \
  "$IMAGE" \
  --config /etc/ru41/ru41.toml --db /data/state.db schedule run campusrun >/dev/null

sleep 6
if docker ps --filter "name=$NAME" --format '{{.Status}}' | grep -q Up; then
  echo "[campusrun] 容器已起：$(docker ps --filter "name=$NAME" --format '{{.Status}}')"
  echo "[campusrun] 只读轮询 + 事后复查；**不会自动提交跑步**（提交要你明确下命令）"
else
  echo "[campusrun] 容器没起来，看 docker logs $NAME" >&2
  exit 1
fi
