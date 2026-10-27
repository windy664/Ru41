#!/usr/bin/env bash
# 快速镜像构建：容器内净室编译（保证 glibc 与运行时镜像兼容），
# 再用 Dockerfile.patch 只替换二进制与 modules，跳过全量镜像构建。
#
# 构建纪律（治理）：只从 HEAD 构建，不吃工作区。
# 在专用 git worktree（默认 ~/Project/Ru41-build）里 reset --hard 到 HEAD，
# 未提交的半成品永远进不了部署；target/ 缓存保留在 worktree 里保持增量速度。
#
# 用法：./deploy/build-fast.sh        # 构建 ru41:local
#       PUSH=1 ./deploy/build-fast.sh # 构建并推送镜像
set -euo pipefail
cd "$(dirname "$0")/.."

BUILD_DIR="${RU41_BUILD_DIR:-$HOME/Project/Ru41-build}"
COMMIT="$(git rev-parse HEAD)"
BUILDER=ru41-builder-$$

cleanup() { docker rm -f "$BUILDER" >/dev/null 2>&1 || true; }
trap cleanup EXIT

if [[ "${RU41_BUILD_FROM:-head}" == "worktree" ]]; then
  # 逃生舱：从当前工作区构建（调试用，不代表可部署状态）。
  BUILD_DIR="$PWD"
  echo "[build-fast] WARNING: building from WORKING TREE (uncommitted state included)"
else
  PREV_REF="$(git -C "$BUILD_DIR" symbolic-ref -q --short HEAD 2>/dev/null || true)"
  if [ ! -e "$BUILD_DIR/.git" ]; then
    git worktree add --detach "$BUILD_DIR" "$COMMIT"
  else
    git -C "$BUILD_DIR" checkout -q --detach "$COMMIT" 2>/dev/null || true
    git -C "$BUILD_DIR" reset -q --hard "$COMMIT"
    # 清掉工作区残留（未跟踪文件），但保留 target/ 增量缓存。
    git -C "$BUILD_DIR" clean -q -fd -e target
  fi
  echo "[build-fast] building HEAD=$COMMIT in $BUILD_DIR"
fi

docker run --name "$BUILDER" \
  -v "$BUILD_DIR":/build \
  -v "$HOME/.cargo/registry:/usr/local/cargo/registry" \
  -e RU41_GIT_HASH_OVERRIDE="$COMMIT" \
  -w /build rust:1.98-trixie \
  cargo build --locked --release --bin ru41

docker build -f "$BUILD_DIR/deploy/Dockerfile.patch" -t ru41:local "$BUILD_DIR"

if [[ "${PUSH:-0}" == "1" ]]; then
  python3 "$HOME/bin/ru41-push-image.py"
  # skills 随镜像外挂在服务器 /etc/ru41/modules（bind mount 自 ~/ru41-mimo-smoke/modules），
  # 不随 Dockerfile.patch 更新，这里显式同步并重启 worker 让新 skill 生效。
  TARBALL="$(mktemp /tmp/ru41-modules-XXXXXX.tgz)"
  tar czf "$TARBALL" -C "$BUILD_DIR/src/modules" .
  python3 "$HOME/bin/ru41-scp.py" "$TARBALL" /tmp/ru41-modules.tgz
  # 同步后重建 $CODEX_HOME/skills 软链：codex 原生技能发现只看这个目录
  # （run_dir/.agents/skills 的 materialize 对 codex 不可见，2026-10-27 实证）。
  python3 "$HOME/bin/ru41-ssh-run2.py" \
    'tar xzf /tmp/ru41-modules.tgz -C /home/windy2/ru41-mimo-smoke/modules && mkdir -p /home/windy2/ru41-mimo-smoke/data/codex/skills && cd /home/windy2/ru41-mimo-smoke/data/codex/skills && for d in /home/windy2/ru41-mimo-smoke/modules/*/skills/*/; do name=$(basename "$d"); ln -sfn "/etc/ru41/modules/$(basename $(dirname $(dirname $d)))/skills/$name" "$name"; done && docker restart ru41-mimo-worker'
  rm -f "$TARBALL"
fi
# 恢复原分支：detached 只是构建姿态，别把后续提交留在游离头上。
if [ -n "${PREV_REF:-}" ]; then
  git -C "$BUILD_DIR" checkout -q "$PREV_REF" 2>/dev/null || true
fi
echo "ru41:local ready ($COMMIT)"
