#!/usr/bin/env bash
# 全插件版本对齐构建：宿主每换一个 commit，所有插件必须同 commit 重编
# （git hash 握手）。本脚本一次跑完：净室编译全部插件 → 发服务器插件目录。
#
# 用法：
#   ./deploy/plugins-build-all.sh          # 只编译（产物在 plugins/*/target-docker、templates/plugin-hello/target-docker）
#   PUSH=1 ./deploy/plugins-build-all.sh   # 编译并 scp 到服务器 /home/windy2/ru41-plugins/
set -euo pipefail
cd "$(dirname "$0")/.."

COMMIT="$(git rev-parse HEAD)"
REGISTRY="$HOME/.cargo/registry"
SSH_RUN="$HOME/bin/ru41-ssh-run2.py"
SCP="$HOME/bin/ru41-scp.py"
PLUGIN_DIR_ON_SERVER="/home/windy2/ru41-plugins"

# (crate 目录, 产物 so 文件名)
PLUGINS=(
  "plugins/competitions:libru41_plugin_competitions.so"
  "templates/plugin-hello:libru41_plugin_hello.so"
)

for entry in "${PLUGINS[@]}"; do
  dir="${entry%%:*}"
  so="${entry##*:}"
  echo "[plugins-build] $dir @ $COMMIT"
  docker run --rm \
    -v "$PWD":/build \
    -v "$REGISTRY":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR="/build/$dir/target-docker" \
    -e RU41_GIT_HASH_OVERRIDE="$COMMIT" \
    -w "/build/$dir" rust:1.98-trixie \
    cargo build --release
  if [[ "${PUSH:-0}" == "1" ]]; then
    python3 "$SCP" "$dir/target-docker/release/$so" "$PLUGIN_DIR_ON_SERVER/$so"
  fi
done
echo "[plugins-build] done ($COMMIT)"
