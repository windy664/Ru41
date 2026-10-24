#!/usr/bin/env bash
# ru41 容器滚动重建：把漂移的容器切到 ru41:local，保留原启动配置
# （env/挂载/网络/重启策略逐字段从 docker inspect 恢复），并接入 ru41-net。
#
# 用法：
#   ./deploy/roll.sh                 # 滚动全部已知 ru41 容器
#   ./deploy/roll.sh ru41-rss ...    # 只滚动指定容器
#
# 安全：重建前把每个容器的完整 inspect 备份到 ~/roll-backup-<时间戳>/，
# 回滚 = 用备份的 inspect 重建 + 旧镜像（镜像标签未被本脚本删除）。
set -euo pipefail

TARGET_IMAGE="${RU41_IMAGE:-ru41:local}"
EXTRA_NET="${RU41_NET:-ru41-net}"
BACKUP="$HOME/roll-backup-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"

if [[ $# -gt 0 ]]; then
  NAMES=("$@")
else
  NAMES=(
    ru41-qq ru41-ecard ru41-jwxt ru41-chaoxing ru41-chaoxing-work
    ru41-zhihuishu ru41-blog ru41-todo-remind ru41-class-remind
    ru41-mimo-worker ru41-rss ru41-projects
  )
fi

NEW_ID="$(docker image inspect "$TARGET_IMAGE" --format '{{.Id}}')"
FAILED=0

for name in "${NAMES[@]}"; do
  if ! docker inspect "$name" > "$BACKUP/$name.json" 2>/dev/null; then
    echo "SKIP $name: 容器不存在"
    continue
  fi
  CUR_ID="$(jq -r '.[0].Image' "$BACKUP/$name.json")"
  if [[ "$CUR_ID" == "$NEW_ID" && "${RU41_ROLL_FORCE:-0}" != "1" ]]; then
    echo "SKIP $name: 已是 $TARGET_IMAGE"
    continue
  fi

  # 从 inspect 逐字段重建启动参数。
  mapfile -t ARGS < <(jq -r '
    .[0] as $c
    | ["--restart", ($c.HostConfig.RestartPolicy.Name // "no")]
    + (if $c.Config.User != "" then ["--user", $c.Config.User] else [] end)
    + (if $c.Config.WorkingDir != "" then ["-w", $c.Config.WorkingDir] else [] end)
    + (($c.Config.Env // []) | map("--env", .))
    + ($c.Mounts | map("-v", .Source + ":" + .Destination + (if .RW then "" else ":ro" end)))
    + (if (($c.HostConfig.NetworkMode // "bridge") != "default")
           and (($c.HostConfig.NetworkMode // "bridge") != "bridge")
         then ["--network", ($c.HostConfig.NetworkMode // "bridge")] else [] end)
    + (($c.HostConfig.ExtraHosts // []) | map("--add-host", .))
    | .[]
  ' "$BACKUP/$name.json")
  ENTRYPOINT="$(jq -r '.[0].Config.Entrypoint // empty | if type == "array" then .[0] else . end' "$BACKUP/$name.json")"
  mapfile -t CMD < <(jq -r '.[0].Config.Cmd[]?' "$BACKUP/$name.json")

  docker stop "$name" > /dev/null
  docker rm "$name" > /dev/null
  if [[ -n "$ENTRYPOINT" ]]; then
    docker run -d --name "$name" "${ARGS[@]}" --entrypoint "$ENTRYPOINT" "$TARGET_IMAGE" "${CMD[@]}" > /dev/null
  else
    docker run -d --name "$name" "${ARGS[@]}" "$TARGET_IMAGE" "${CMD[@]}" > /dev/null
  fi
  docker network connect "$EXTRA_NET" "$name" 2>/dev/null || true
  echo "OK   $name: ${CUR_ID:7:12} -> ${NEW_ID:7:12} (+$EXTRA_NET)"
done

echo "backup: $BACKUP"
exit "$FAILED"
