#!/usr/bin/env bash
# 重建指定容器并从环境变量里剔除指定项（默认 ZHIPU_API_KEY）。
#
# 用途：GLM 供应方下线后，容器里还留着 ZHIPU_API_KEY 环境变量，
# 而 roll.sh 是从 inspect 原样恢复 env 的，没法删单个变量——只能按 inspect
# 重建一次，env 里过滤掉它。
#
# 用法：./deploy/roll-drop-env.sh ru41-mimo-worker ru41-obsidian [ENV_NAME...]
set -euo pipefail

TARGET_IMAGE="${RU41_IMAGE:-ru41:local}"
EXTRA_NET="${RU41_NET:-ru41-net}"
DROP_ENV=()
NAMES=()

for arg in "$@"; do
  case "$arg" in
    *=*|ZHIPU_API_KEY|MINIMAX_API_KEY|CODEX_HOME) DROP_ENV+=("$arg");;
    *) NAMES+=("$arg");;
  esac
done

if [[ ${#DROP_ENV[@]} -eq 0 ]]; then
  DROP_ENV=("ZHIPU_API_KEY")
fi
if [[ ${#NAMES[@]} -eq 0 ]]; then
  echo "用法：$0 <容器名>... [要剔除的环境变量名...]" >&2
  exit 1
fi

BACKUP="$HOME/roll-backup-dropenv-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"

for name in "${NAMES[@]}"; do
  if ! docker inspect "$name" > "$BACKUP/$name.json" 2>/dev/null; then
    echo "SKIP $name: 容器不存在"
    continue
  fi

  mapfile -t ARGS < <(jq -r --argjson drop "$(printf '%s\n' "${DROP_ENV[@]}" | jq -R . | jq -s .)" '
    .[0] as $c
    | ["--restart", ($c.HostConfig.RestartPolicy.Name // "no")]
    + (if $c.Config.User != "" then ["--user", $c.Config.User] else [] end)
    + (if $c.Config.WorkingDir != "" then ["-w", $c.Config.WorkingDir] else [] end)
    + (($c.Config.Env // [])
       | map(select((split("=")[0]) as $n | ($drop | index($n)) | not))
       | map("--env", .))
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
  echo "OK   $name: 重建完成，剔除 ${DROP_ENV[*]}"
done

echo "backup: $BACKUP"
