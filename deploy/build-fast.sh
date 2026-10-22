#!/usr/bin/env bash
# 快速镜像构建：容器内净室编译（保证 glibc 与运行时镜像兼容），
# 再用 Dockerfile.patch 只替换二进制与 modules，跳过全量镜像构建。
#
# 背景：宿主直接 cargo build 出的二进制链接了宿主 glibc（如 Ubuntu 2.43），
# 而运行时镜像是 debian bookworm（glibc 2.36），直接 COPY 进去会
# 报 `GLIBC_2.39 not found` 启动失败。必须在 rust:1.98-bookworm
# 里编译（Dockerfile 的 builder 阶段同款环境）。
#
# 用法：./deploy/build-fast.sh        # 构建 ru41:local
#       PUSH=1 ./deploy/build-fast.sh # 构建并推送镜像
set -euo pipefail
cd "$(dirname "$0")/.."

BUILDER=ru41-builder-$$

cleanup() { docker rm -f "$BUILDER" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker run --name "$BUILDER" \
  -v "$PWD":/build \
  -v "$HOME/.cargo/registry:/usr/local/cargo/registry" \
  -w /build rust:1.98-bookworm \
  cargo build --locked --release --bin ru41

docker build -f deploy/Dockerfile.patch -t ru41:local .

if [[ "${PUSH:-0}" == "1" ]]; then
  python3 /tmp/ru41-push-image.py
fi
echo "ru41:local ready"
