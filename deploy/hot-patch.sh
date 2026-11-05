#!/usr/bin/env bash
# 插件热更：本地编译 → 传 .so → 重启对应容器。
#
# 什么时候用：只改了 plugins/<名>/ 里的代码（没动 src/ 核心、没改插件的技能包）。
# 这时不必等 CI 出镜像——整镜像 2.7GB，搬运 + 构建要 10 分钟以上，
# 而插件是 cdylib，容器启动时从 /etc/ru41/plugins 加载，替换 .so 重启即可。
#
# 什么时候不能用：
#   - 改了 src/（宿主二进制变了，插件 ABI 与旧二进制对不上，必须走完整 CI 部署）；
#   - 改了 plugins/<名>/skills/（技能包打在镜像的 /usr/share/ru41/plugins/，热更带不上）；
#   - 插件有配套的 compose 服务需要重建（那是 sidecar，不在此脚本范围）。
#
# 用法：deploy/hot-patch.sh <插件名> [容器名 ...]
#   deploy/hot-patch.sh lecture
#   deploy/hot-patch.sh zhihuishu ru41-zhihuishu

set -euo pipefail

name="${1:-}"
if [[ -z "$name" ]]; then
  echo "用法: $0 <插件名> [容器名 ...]" >&2
  exit 2
fi
shift || true

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
server="${RU41_SERVER:-windy2@192.168.1.223}"
plugin_dir="$repo_root/plugins/$name"
so="$repo_root/plugins/target-plugins/release/libru41_plugin_${name}.so"
containers=("$@")
if [[ ${#containers[@]} -eq 0 ]]; then
  containers=("ru41-$name")
fi

if [[ ! -d "$plugin_dir" ]]; then
  echo "没有这个插件目录: $plugin_dir" >&2
  exit 2
fi

echo "==> 编译 $name（release + plugin-entry）"
cd "$plugin_dir"
cargo build --release --features plugin-entry

if [[ ! -f "$so" ]]; then
  echo "产物不存在: $so" >&2
  exit 1
fi

echo "==> 传输 $(basename "$so") 到 $server"
scp "$so" "$server:~/ru41-mimo-smoke/plugins/"

# 技能包也在宿主目录（挂载到 /usr/share/ru41/plugins），改了就一并同步。
# 目录形状必须是 <插件>/skills/<技能>/SKILL.md —— 加载器按这个结构找。
if [[ -d "$plugin_dir/skills" ]]; then
  echo "==> 同步技能包 $name"
  ssh "$server" "mkdir -p ~/ru41-mimo-smoke/skills/$name/skills"
  # 按内容拷（scp 的 "src/." 语义不可靠，会多套一层同名目录）。
  scp -r "$plugin_dir/skills/"* "$server:~/ru41-mimo-smoke/skills/$name/skills/"
fi

echo "==> 重启: ${containers[*]}"
ssh "$server" "docker restart ${containers[*]} >/dev/null"

for container in "${containers[@]}"; do
  echo "==> $container 最近日志"
  ssh "$server" "docker logs --since 30s '$container' 2>&1 | tail -5"
done

echo "==> 热更完成（未过 CI；核心改动请走 push + CI + roll）"
