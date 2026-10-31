#!/usr/bin/env bash
# 全插件构建：净室编译全部插件 → 发服务器插件目录。
# 握手 v2：硬门是 ru41-plugin-api crate 版本 + deps stamp（不是 git commit），
# 只有边界类型或 tokio/serde 等依赖版本变化才必须重编插件；
# 各插件 Cargo.lock 从仓库根同步（deps stamp 的两侧一致性来源）。
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
  "plugins/papers:libru41_plugin_papers.so"
  "plugins/rss:libru41_plugin_rss.so"
  "plugins/pagewatch:libru41_plugin_pagewatch.so"
  "plugins/jobs:libru41_plugin_jobs.so"
  "plugins/programs:libru41_plugin_programs.so"
  "plugins/research:libru41_plugin_research.so"
  "plugins/zhihuishu:libru41_plugin_zhihuishu.so"
  "plugins/homework:libru41_plugin_homework.so"
  "plugins/ecard:libru41_plugin_ecard.so"
  "plugins/compshare:libru41_plugin_compshare.so"
  "plugins/blog:libru41_plugin_blog.so"
  "plugins/projects:libru41_plugin_projects.so"
  "plugins/lecture:libru41_plugin_lecture.so"
)

for entry in "${PLUGINS[@]}"; do
  dir="${entry%%:*}"
  so="${entry##*:}"
  echo "[plugins-build] $dir @ $COMMIT"
  # lockfile 从根同步：deps stamp 以根 lock 为准，插件必须解析到同一组版本。
  cp Cargo.lock "$dir/Cargo.lock"
  docker run --rm \
    -v "$PWD":/build \
    -v "$REGISTRY":/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR="/build/$dir/target-docker" \
    -e RU41_GIT_HASH_OVERRIDE="$COMMIT" \
    -w "/build/$dir" rust:1.98-trixie \
    cargo build --release --features plugin-entry
  if [[ "${PUSH:-0}" == "1" ]]; then
    python3 "$SCP" "$dir/target-docker/release/$so" "$PLUGIN_DIR_ON_SERVER/$so"
  fi
done
echo "[plugins-build] done ($COMMIT)"
