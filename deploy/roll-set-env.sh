#!/usr/bin/env bash
# 重建指定容器并补上环境变量（值从参考容器里抄，不必把密钥写进命令行）。
#
# 用途：obsidian 容器要跑 AI 语义合并，需要底座供应商的密钥与出网代理，
# 而这些变量原本只在 worker 容器里——roll.sh 从 inspect 恢复 env，
# 不会凭空多给，所以得按 inspect 重���一次。
#
# 用法：./deploy/roll-set-env.sh <容器名> <参考容器名> [变量名...]
#   例：./deploy/roll-set-env.sh ru41-obsidian ru41-mimo-worker MINIMAX_API_KEY HTTP_PROXY HTTPS_PROXY NO_PROXY
set -euo pipefail

TARGET_IMAGE="${RU41_IMAGE:-ru41:local}"
EXTRA_NET="${RU41_NET:-ru41-net}"

if [[ $# -lt 3 ]]; then
  echo "用法：$0 <容器名> <参考容器名> [变量名...]" >&2
  exit 1
fi

NAME="$1"
SOURCE="$2"
shift 2
WANTED=("$@")

SRC_ENV="$(docker inspect "$SOURCE" --format '{{range .Config.Env}}{{println .}}{{end}}')"
ARGS=()
for key in "${WANTED[@]}"; do
  value="$(printf '%s\n' "$SRC_ENV" | sed -n "s/^${key}=//p" | head -1)"
  if [[ -z "$value" ]]; then
    echo "参考容器 $SOURCE 没有变量 $key" >&2
    exit 1
  fi
  ARGS+=(--env "${key}=${value}")
done

BACKUP="$HOME/roll-backup-setenv-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$BACKUP"
docker inspect "$NAME" > "$BACKUP/$NAME.json"

mapfile -t BASE_ARGS < <(jq -r '
  .[0] as $c
  | ["--restart", ($c.HostConfig.RestartPolicy.Name // "no")]
  + (if $c.Config.User != "" then ["--user", $c.Config.User] else [] end)
  + (if $c.Config.WorkingDir != "" then ["-w", $c.Config.WorkingDir] else [] end)
  + (($c.Config.Env // [])
     | map(select(startswith("ZHIPU_API_KEY=") | not))
     | map("--env", .))
  + ($c.Mounts | map("-v", .Source + ":" + .Destination + (if .RW then "" else ":ro" end)))
  + (if (($c.HostConfig.NetworkMode // "bridge") != "default")
         and (($c.HostConfig.NetworkMode // "bridge") != "bridge")
       then ["--network", ($c.HostConfig.NetworkMode // "bridge")] else [] end)
  + (($c.HostConfig.ExtraHosts // []) | map("--add-host", .))
  | .[]
' "$BACKUP/$NAME.json")

# ⚠️ 上面那段 jq 的结果必须**真的被检查**：
# 进程替换里的 jq 失败不会让 set -e 触发，一旦 jq 写错（比如括号少一个），
# BASE_ARGS 会是空数组 → 重建出的容器**丢掉全部环境变量**（密钥、代理全没），
# 而且没有任何报错。2026-11-17 夜里就是这么把 worker 和 telegram 弄挂的。
# 所以这里自查：备份里有 env、恢复出来却没有 → 中止，别重建。
EXPECTED_ENV="$(jq -r '.[0].Config.Env // [] | length' "$BACKUP/$NAME.json")"
GOT_ENV="$(jq -r '.[0].Config.Env // [] | map(select(startswith("ZHIPU_API_KEY=") | not)) | length' "$BACKUP/$NAME.json")"
if [[ "${GOT_ENV}" -gt 0 && "${#BASE_ARGS[@]}" -lt 3 ]]; then
  echo "中止：备份里有 ${GOT_ENV} 个环境变量，但只恢复出 ${#BASE_ARGS[@]} 项参数（jq 可能没跑对）" >&2
  echo "     备份在 $BACKUP（容器未动）" >&2
  exit 1
fi
if [[ "${EXPECTED_ENV}" -gt 0 && "${GOT_ENV}" -eq 0 ]]; then
  echo "中止：环境变量被整条过滤掉了（jq 过滤写错？）" >&2
  exit 1
fi

ENTRYPOINT="$(jq -r '.[0].Config.Entrypoint // empty | if type == "array" then .[0] else . end' "$BACKUP/$NAME.json")"
mapfile -t CMD < <(jq -r '.[0].Config.Cmd[]?' "$BACKUP/$NAME.json")

docker stop "$NAME" > /dev/null
docker rm "$NAME" > /dev/null
if [[ -n "$ENTRYPOINT" ]]; then
  docker run -d --name "$NAME" "${BASE_ARGS[@]}" "${ARGS[@]}" --entrypoint "$ENTRYPOINT" "$TARGET_IMAGE" "${CMD[@]}" > /dev/null
else
  docker run -d --name "$NAME" "${BASE_ARGS[@]}" "${ARGS[@]}" "$TARGET_IMAGE" "${CMD[@]}" > /dev/null
fi
docker network connect "$EXTRA_NET" "$NAME" 2>/dev/null || true
echo "OK   $NAME: 重建完成，补上 ${WANTED[*]}"
echo "backup: $BACKUP"
