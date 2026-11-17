#!/usr/bin/env bash
# 插件边界守卫：根 Cargo.lock 里的 ru41-plugin-api 版本必须与 crate 版本一致。
#
# 为什么守这条：插件是 cdylib，按 api crate 的公开类型布局编译。镜像构建
# （Dockerfile 的 plugins 阶段）会把**根 Cargo.lock** 复制进每个插件目录再编译，
# 所以「根 lock 与 crate 版本一致」才是真正决定 .so 边界布局的东西。
# 根 lock 落后于 crate → 插件按旧布局编出来 → 宿主握手指纹不匹配 →
# 整批插件被跳过（2026-11-16 生产事故：24 个插件全部「magic 不匹配」）。
#
# 各插件自己的 Cargo.lock 只是构建产物（每次构建都会被根 lock 覆盖），
# 版本天然会漂移，这里只提示、不拦截。
set -euo pipefail
cd "$(dirname "$0")/.."

crate_version="$(grep -m1 '^version' crates/ru41-plugin-api/Cargo.toml | cut -d'"' -f2)"
if [[ -z "$crate_version" ]]; then
  echo "读不到 crates/ru41-plugin-api 的版本"
  exit 1
fi

lock_version="$(grep -A2 'name = "ru41-plugin-api"' Cargo.lock | grep -m1 '^version' | cut -d'"' -f2)"
if [[ "$lock_version" != "$crate_version" ]]; then
  echo "根 Cargo.lock 的 ru41-plugin-api 版本落后于 crate："
  echo "  Cargo.lock = $lock_version，crates/ru41-plugin-api = $crate_version"
  echo "  镜像构建会把根 lock 复制进各插件目录 → 插件会按旧边界布局编译。"
  echo "  修复：cargo update -p ru41-plugin-api --offline"
  exit 1
fi

drifted=0
for manifest in plugins/*/Cargo.toml; do
  dir="$(dirname "$manifest")"
  lock="$dir/Cargo.lock"
  [[ -f "$lock" ]] || continue
  locked="$(grep -A2 'name = "ru41-plugin-api"' "$lock" | grep -m1 '^version' | cut -d'"' -f2)"
  if [[ "$locked" != "$crate_version" ]]; then
    echo "提示：$dir 的锁文件是 $locked（构建时会被根 lock 覆盖，不影响镜像）"
    drifted=$((drifted + 1))
  fi
done

echo "根锁文件与 plugin-api 版本一致: $crate_version（$drifted 个插件锁文件待构建时刷新）"
