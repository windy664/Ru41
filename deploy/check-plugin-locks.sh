#!/usr/bin/env bash
# 插件锁文件守卫：各插件的 Cargo.lock 必须与 crates/ru41-plugin-api 的版本一致。
#
# 为什么必须有：插件是 cdylib，按 api crate 的公开类型布局编译。api 升版后
# 插件若还锁着旧版本，.so 就按旧布局编出来——宿主握手会拒绝加载（指纹硬门），
# 表现为「24 个插件全部被跳过」，而不是编译期报错（2026-11-16 生产事故）。
#
# 用法：bash deploy/check-plugin-locks.sh
set -euo pipefail
cd "$(dirname "$0")/.."

api_version="$(grep -m1 '^version' crates/ru41-plugin-api/Cargo.toml | cut -d'"' -f2)"
if [[ -z "$api_version" ]]; then
  echo "读不到 crates/ru41-plugin-api 的版本"
  exit 1
fi

failed=0
checked=0
for manifest in plugins/*/Cargo.toml; do
  dir="$(dirname "$manifest")"
  lock="$dir/Cargo.lock"
  [[ -f "$lock" ]] || { echo "缺锁文件: $dir"; failed=1; continue; }
  locked="$(grep -A2 'name = "ru41-plugin-api"' "$lock" | grep -m1 '^version' | cut -d'"' -f2)"
  checked=$((checked + 1))
  if [[ "$locked" != "$api_version" ]]; then
    echo "锁文件版本不符: $dir -> $locked（应为 $api_version）"
    failed=1
  fi
done

if [[ "$failed" != 0 ]]; then
  echo
  echo "修复：在各插件目录执行 cargo update -p ru41-plugin-api --offline 后提交锁文件。"
  exit 1
fi
echo "插件锁文件版本一致: ru41-plugin-api $api_version（$checked 个插件）"
