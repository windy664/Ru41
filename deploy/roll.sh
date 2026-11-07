#!/usr/bin/env bash
# ru41 容器滚动重建：把漂移的容器切到 ru41:local，保留原启动配置
# （env/挂载/网络/重启策略逐字段从 docker inspect 恢复），并接入 ru41-net。
#
# 用法：
#   ./deploy/roll.sh                 # 滚动全部 ru41 容器（自动发现，无手写名单）
#   ./deploy/roll.sh ru41-rss ...    # 只滚动指定容器
#
# 自动发现规则：容器名以 ru41- 开头 且 entrypoint 是 /usr/local/bin/ru41。
# 不依赖任何手写列表——新附属上线（新容器）自动被后续滚动覆盖，
# 镜像被 pin 成无标签 ID 的掉队容器也能凭 entrypoint 识别回来。
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
  NAMES=()
  while read -r name; do
    [[ "$name" == ru41-* ]] || continue
    ep="$(docker inspect "$name" --format '{{json .Config.Entrypoint}}' 2>/dev/null || true)"
    [[ "$ep" == *"/usr/local/bin/ru41"* ]] && NAMES+=("$name")
  done < <(docker ps -a --format '{{.Names}}')
fi

if [[ ${#NAMES[@]} -eq 0 ]]; then
  echo "没有发现任何 ru41 容器" >&2
  exit 1
fi
echo "目标容器（${#NAMES[@]} 个）: ${NAMES[*]}"

NEW_ID="$(docker image inspect "$TARGET_IMAGE" --format '{{.Id}}')"
FAILED=0

for name in "${NAMES[@]}"; do
  if ! docker inspect "$name" > "$BACKUP/$name.json" 2>/dev/null; then
    echo "SKIP $name: 容器不存在"
    continue
  fi
  CUR_ID="$(jq -r '.[0].Image' "$BACKUP/$name.json")"
  NEW_ID_TARGET="$TARGET_IMAGE"
  # 镜像没变也要重建的情况：缺技能包挂载（挂载是后加的，inspect 里还没有）。
  SKILLS_DIR="$HOME/ru41-mimo-smoke/skills"
  NEEDS_RECREATE=0
  if [[ -d "$SKILLS_DIR" ]] && ! jq -e --arg src "$SKILLS_DIR" \
      '.[0].Mounts[]? | select(.Source == $src and .Destination == "/usr/share/ru41/plugins")' \
      "$BACKUP/$name.json" > /dev/null; then
    NEEDS_RECREATE=1
  fi
  if [[ "$CUR_ID" == "$NEW_ID" && "$NEEDS_RECREATE" == "0" && "${RU41_ROLL_FORCE:-0}" != "1" ]]; then
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
    + (if ($c.HostConfig.NanoCpus // 0) > 0
           then ["--cpus", (($c.HostConfig.NanoCpus / 1000000000) | tostring)] else [] end)
    + (if ($c.HostConfig.Memory // 0) > 0
           then ["--memory", (($c.HostConfig.Memory / 1048576) | tostring),
                 "--memory-swap", (($c.HostConfig.Memory / 1048576) | tostring)]
           else [] end)
    | .[]
  ' "$BACKUP/$name.json")
  ENTRYPOINT="$(jq -r '.[0].Config.Entrypoint // empty | if type == "array" then .[0] else . end' "$BACKUP/$name.json")"
  mapfile -t CMD < <(jq -r '.[0].Config.Cmd[]?' "$BACKUP/$name.json")

  # 技能包挂载：宿主目录盖住镜像里的 /usr/share/ru41/plugins，
  # 这样改技能包也能热更（只 scp 技能目录，不用重建镜像）。
  # 宿主目录首次由镜像内容初始化（docker cp 镜像里的 /usr/share/ru41/plugins/.）。
  SKILLS_DIR="$HOME/ru41-mimo-smoke/skills"
  if [[ -d "$SKILLS_DIR" ]] && ! printf '%s\n' "${ARGS[@]}" | grep -qF "$SKILLS_DIR:/usr/share/ru41/plugins"; then
    ARGS+=(-v "$SKILLS_DIR:/usr/share/ru41/plugins:ro")
  fi

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
