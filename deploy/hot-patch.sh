#!/usr/bin/env bash
# 插件热更：容器内编译 → 传 .so → 重启对应容器。
#
# 什么时候用：只改了 plugins/<名>/ 里的代码（没动 src/ 核心、没改插件的技能包）。
# 这时不必等 CI 出镜像——整镜像 2.7GB，搬运 + 构建要 10 分钟以上，
# 而插件是 cdylib，容器启动时从 /etc/ru41/plugins 加载，替换 .so 重启即可。
#
# 为什么在容器里编译：.so 是 cdylib，和宿主二进制在同一个进程里跑。
# 本机编的 .so（Ubuntu/glibc）与镜像里的（Debian trixie）不是同一套运行库，
# 线上出现过段错误。统一用 rust:1.98-trixie（镜像 Dockerfile 同款）编译，
# 并沿用仓库根 Cargo.lock（插件 crate 依赖主 crate，锁文件必须一致）。
#
# 什么时候不能用：
#   - 改了 src/（宿主二进制变了，插件 ABI 与旧二进制对不上，必须走完整 CI 部署）；
#   - 改了插件的 skills/（技能包在镜像的 /usr/share/ru41/plugins，已改为宿主目录挂载，
#     但仍需与镜像内版本一致时以 CI 为准）；
#   - 插件有配套的 compose 服务需要重建（那是 sidecar，不在此脚本范围）。
#
# 用法：deploy/hot-patch.sh <插件名> [容器名 ...]
#   deploy/hot-patch.sh lecture
#   deploy/hot-patch.sh zhihuishu ru41-zhihuishu

set -euo pipefail

FORCE=0
if [[ "${1:-}" == "--force" ]]; then
  FORCE=1
  shift
fi
name="${1:-}"
if [[ -z "$name" ]]; then
  echo "用法: $0 [--force] <插件名> [容器名 ...]" >&2
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

# 护栏：热更只适用于纯插件改动。核心（src/ crates/ migrations/ 根 Cargo.toml）
# 变了就意味着宿主二进制要换，插件 ABI 与旧二进制对不上——必须走完整 CI 部署。
# 除非显式 --force（明知故犯时才用）。
core_changes="$(git -C "$repo_root" status --porcelain 2>/dev/null |
  awk '{print $NF}' |
  grep -E '^(src/|crates/|migrations/|Cargo\.toml|Cargo\.lock)' || true)"
if [[ -n "$core_changes" && "$FORCE" != "1" ]]; then
  echo "检测到核心改动，热更不适用（宿主二进制要换，插件 ABI 对不上）：" >&2
  echo "$core_changes" | sed 's/^/  /' >&2
  echo "请走完整链：push → CI → 搬镜像 → roll（确实要强行热更改加 --force）。" >&2
  exit 3
fi

echo "==> 在 rust:1.98-trixie 里编译 $name（与镜像同一工具链）"
# 内层脚本用单引号包裹：外层双引号会被内层 " 提前截断，导致 export 之后
# 的内容变成命令（构建静默变成空跑，部署的还是旧 .so）。
docker run --rm \
  -v "$repo_root:/build" \
  -v "$repo_root/plugins/target-plugins:/build/plugins/target-plugins" \
  -w /build/plugins/"$name" \
  rust:1.98-trixie \
  bash -c '
    set -e
    export PATH=/usr/local/cargo/bin:$PATH
    # plugins/.cargo/config.toml 要求 clang+lld，镜像构建里被 .dockerignore 排除了，
    # 这里同样用默认链接器（与 CI 产物一致）。RUSTFLAGS 优先级高于 build.rustflags。
    export RUSTFLAGS="-C opt-level=3"
    cp ../../Cargo.lock Cargo.lock
    CARGO_TARGET_DIR=/build/plugins/target-plugins cargo build --release --features plugin-entry
    # 产物必须在宿主可见（$so 从宿主路径取），所以 target 目录挂宿主路径而非命名卷。
    # 构建会把插件目录的 Cargo.lock 改写（与根锁对齐），别留脏改动。
    git -C /build checkout -- "plugins/$1/Cargo.lock" 2>/dev/null || true
  ' _ "$name"

if [[ ! -f "$so" ]]; then
  echo "产物不存在: $so" >&2
  exit 1
fi

echo "==> 传输 $(basename "$so") 到 $server"
scp "$so" "$server:~/ru41-mimo-smoke/plugins/"

# 技能包也在宿主目录（挂载到 /usr/share/ru41/plugins），改了就一并同步。
# 目录形状必须是 <插件>/skills/<技能>/SKILL.md —— 加载器按这个结构找。
# 技能放在插件根的 skills/ 或 src/skills/ 两种位置都认。
skill_src=""
for candidate in "$plugin_dir/skills" "$plugin_dir/src/skills"; do
  if [[ -d "$candidate" ]]; then
    skill_src="$candidate"
    break
  fi
done
if [[ -n "$skill_src" ]]; then
  echo "==> 同步技能包 $name（$(basename "$(dirname "$skill_src")")/skills）"
  ssh "$server" "mkdir -p ~/ru41-mimo-smoke/skills/$name/skills"
  # 按内容拷（scp 的 "src/." 语义不可靠，会多套一层同名目录）。
  scp -r "$skill_src/"* "$server:~/ru41-mimo-smoke/skills/$name/skills/"
fi

echo "==> 重启: ${containers[*]}"
ssh "$server" "docker restart ${containers[*]} >/dev/null"

for container in "${containers[@]}"; do
  echo "==> $container 最近日志"
  ssh "$server" "docker logs --since 30s '$container' 2>&1 | tail -5"
done

echo "==> 热更完成（未过 CI；核心改动请走 push + CI + roll）"
