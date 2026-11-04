#!/usr/bin/env bash
# 一次性：给全部 ru41 容器注入出口代理 env（P3+ observe-only 审计的前提）。
# 仅存量容器需要跑一次；compose.yaml 已含同名 env，新容器天然携带；
# 之后 roll.sh 从 inspect 恢复 env，注入自动保留。
# 用法：bash inject-proxy.sh
set -euo pipefail
BACKUP="$HOME/roll-backup-proxy-env-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"
TARGET_IMAGE="${RU41_IMAGE:-ru41:local}"
NEW_ID="$(docker image inspect "$TARGET_IMAGE" --format '{{.Id}}')"

for name in $(docker ps -a --format '{{.Names}}' | grep '^ru41-'); do
  ep="$(docker inspect "$name" --format '{{json .Config.Entrypoint}}' 2>/dev/null || true)"
  [[ "$ep" == *"/usr/local/bin/ru41"* ]] || continue
  docker inspect "$name" > "$BACKUP/$name.json"
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
  docker run -d --name "$name" "${ARGS[@]}" \
    --env HTTP_PROXY=http://172.17.0.1:7890 \
    --env HTTPS_PROXY=http://172.17.0.1:7890 \
    --env NO_PROXY=localhost,127.0.0.1,::1,ru41-browser,172.17.0.1 \
    --entrypoint "$ENTRYPOINT" "$TARGET_IMAGE" "${CMD[@]}" > /dev/null
  docker network connect ru41-net "$name" 2>/dev/null || true
  echo "OK $name"
done
echo "backup: $BACKUP"
