#!/usr/bin/env bash
# 快速镜像构建：容器内净室编译（保证 glibc 与运行时镜像兼容），
# 再用 Dockerfile.patch 只替换二进制与 modules，跳过全量镜像构建。
#
# 构建纪律（治理）：只从 HEAD 构建，不吃工作区。
# 在专用 git worktree（默认 ~/Projects/Ru41-build）里 reset --hard 到 HEAD，
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
    # .target-docker-shared 是容器以 root 创建的，宿主 git 删不动 → 必须排除。
    git -C "$BUILD_DIR" clean -q -fd -e target -e .target-docker-shared -e target-docker
  fi
  echo "[build-fast] building HEAD=$COMMIT in $BUILD_DIR"
fi

# local-embed：生产 RAG 需要 fastembed（默认关是给 musl 静态构建让路）。
# 警告：docker run 的 \ 续行链里禁止插注释行——bash 续行遇注释即终结，
# cargo 会变成独立命令在宿主机裸跑（2026-10-28 GLIBC_2.43 全群崩溃根因）。
docker run --name "$BUILDER" \
  -v "$BUILD_DIR":/build \
  -v "$HOME/.cargo/registry:/usr/local/cargo/registry" \
  -v "$HOME/.cache/ort.pyke.io:/root/.cache/ort.pyke.io" \
  -e RU41_GIT_HASH_OVERRIDE="$COMMIT" \
  -e CARGO_TARGET_DIR=/build/target \
  -w /build rust:1.98-trixie \
  cargo build --locked --release --bin ru41 --features local-embed

# 插件也要重编进 .plugins-dist/（Dockerfile.patch 会 COPY 过去）。
# 不编的后果(2026-11-22 实测): roll.sh 会用镜像里的插件**覆盖**宿主挂载目录
# (「以镜像为准」是既定行为), 补丁镜像若不带插件, 宿主上热更过的 .so 会被
# 回退成镜像里那个旧版 —— rss 插件被 11-18 的旧版冲掉后, 不认识配置里的
# must_report 字段 → ru41-rss 崩溃循环。
# RU41_SKIP_PLUGINS=1 可跳过(明知会回退插件时才用)。
#
# ⚠️ 这里以前还有一段「宿主机 cargo 编插件」的循环, 已删除:
#   1. 它的产物随后会被下面的净室循环 rm -rf 掉, 纯属白干;
#   2. 宿主机 glibc 比 trixie 新, 编出的 .so 进镜像会烂(见下面 GLIBC 守卫那段的实证);
#   3. `for d in plugins/*/` 会把 plugins/target-plugins/ 当成插件目录,
#      那里没有 Cargo.toml → cargo 往上找到根包 → 报
#      「the package 'ru41' does not contain this feature: plugin-entry」直接退出 8
#      (2026-11-22 实测: 只要跑过一次 hot-patch.sh 盘上有 target-plugins/ 就必炸)。

# 守卫：target/ 若被宿主机 cargo 污染（宿主机 glibc 比 trixie 运行时新），
# cargo 可能误判产物新鲜直接复用——部署后容器内 GLIBC_X.XX not found 起不来
# （2026-10-28 实证：宿主机 glibc 2.43 二进制进 trixie(glibc 2.41) 镜像全群 CrashLoop）。
BIN_GLIBC="$(objdump -T "$BUILD_DIR/target/release/ru41" | grep -oE 'GLIBC_[0-9.]+' | sort -Vu | tail -1)"
RT_GLIBC="$(docker run --rm rust:1.98-trixie ldd --version | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
if [ -n "$BIN_GLIBC" ] && [ "$(printf '%s\n%s\n' "$RT_GLIBC" "${BIN_GLIBC#GLIBC_}" | sort -V | tail -1)" != "$RT_GLIBC" ]; then
  echo "[build-fast] FATAL: 二进制需要 $BIN_GLIBC，超过运行时镜像 glibc $RT_GLIBC" >&2
  echo "[build-fast] target/ 疑似被宿主机 cargo 污染——rm -rf $BUILD_DIR/target/release 后重跑" >&2
  exit 1
fi

# 插件净室编译；全部插件共用一个 target（.target-docker-shared），
  # 依赖树只编一次——曾按插件各开 target-docker，13 份重复依赖占 40G。
  # 握手 v2 硬门是 ru41-plugin-api 版本 + deps stamp
  # （不是 git commit），lockfile 从根同步保证两侧解析同一组依赖版本。
  PLUGINS_TARGET=/build/plugins/.target-docker-shared
  # 同样用 Cargo.toml 作锚点（plugins/ 下还有 target-plugins/ 这类目录）。
  for manifest in "$BUILD_DIR"/plugins/*/Cargo.toml; do
    [ -f "$manifest" ] || continue
    dir="$(dirname "$manifest")"
    name="$(basename "$dir")"
    cp "$BUILD_DIR/Cargo.lock" "$dir/Cargo.lock"
    # 用 --manifest-path 而不是 -w plugins/<name>：cargo 的配置发现基于 cwd，
    # 进到 plugins/ 会读到 plugins/.cargo/config.toml（本地开发加速用 clang+lld），
    # 而构建镜像只有 cc 没有 clang → linker not found（2026-11-15 实测）。
    docker run --rm \
      -v "$BUILD_DIR":/build \
      -v "$HOME/.cargo/registry:/usr/local/cargo/registry" \
  -v "$HOME/.cache/ort.pyke.io:/root/.cache/ort.pyke.io" \
      -e CARGO_TARGET_DIR="$PLUGINS_TARGET" \
      -e RU41_GIT_HASH_OVERRIDE="$COMMIT" \
      -w "/build" rust:1.98-trixie \
      cargo build --manifest-path "/build/plugins/$name/Cargo.toml" --release --features plugin-entry
  done
  # 汇集 .so 到独立目录：Dockerfile.patch 只 COPY 这个目录，
  # 避免把共享 target（数 GB）带进构建上下文。
  rm -rf "$BUILD_DIR/.plugins-dist"
  mkdir -p "$BUILD_DIR/.plugins-dist"
  # 按精确文件名收 .so——插件作为对方依赖编译时只在 deps/ 产 rlib，
  # 不会覆盖 release/ 顶层的 cdylib；deps/ 里的中间产物一律不收。
  # 用 Cargo.toml 作锚点，不用 plugins/*/ 目录名：plugins/target-plugins/
  # 与 .target-docker-shared 也是 plugins/ 下的目录，按目录名遍历会把它们当插件
  # （编不出来就报「插件产物缺失 libru41_plugin_target-plugins.so」，2026-11-22 实测）。
  for manifest in "$BUILD_DIR"/plugins/*/Cargo.toml; do
    [ -f "$manifest" ] || continue
    name="$(basename "$(dirname "$manifest")")"
    so="$BUILD_DIR/plugins/.target-docker-shared/release/libru41_plugin_$name.so"
    # cargo 把 crate 名里的连字符换成下划线（ru41-plugin-course-brush →
    # libru41_plugin_course_brush.so），而这里的 $name 是**目录名**。
    # 目录名带连字符时两者对不上 → 两种都试（2026-11-24 实测踩到）。
    # 注：宿主按 .so 文件名过滤插件只在 RU41_PLUGINS 设了白名单时才用得上，
    # 注册名取自插件的 DEF.name，所以文件名本身不影响插件被加载。
    [ -f "$so" ] || so="$BUILD_DIR/plugins/.target-docker-shared/release/libru41_plugin_${name//-/_}.so"
    [ -f "$so" ] || { echo "[build-fast] FATAL: 插件产物缺失 libru41_plugin_$name.so"; exit 1; }
    cp "$so" "$BUILD_DIR/.plugins-dist/"
  done
  ls "$BUILD_DIR/.plugins-dist" | wc -l
  # 验收：每个 .so 必须导出入口符号（feature 漏开/陈旧产物立即暴露）。
  for so in "$BUILD_DIR"/.plugins-dist/libru41_plugin_*.so; do
    nm -D --defined-only "$so" | grep -q ru41_plugin_entry \
      || { echo "[build-fast] FATAL: $so 缺少 ru41_plugin_entry"; exit 1; }
  done

  # 插件技能包也打包进镜像（布局同主 Dockerfile 的 /plugins-skills）。
  # 不打包的后果(2026-11-22): roll.sh 会用镜像里的 /usr/share/ru41/plugins
  # 覆盖宿主 ~/ru41-mimo-smoke/skills —— 改过的 SKILL.md 当场回退成旧版，
  # 表现是「镜像换了、agent 行为没变」。
  rm -rf "$BUILD_DIR/.skills-dist"
  mkdir -p "$BUILD_DIR/.skills-dist"
  for dir in "$BUILD_DIR"/plugins/*/skills; do
    [ -d "$dir" ] || continue
    name="$(basename "$(dirname "$dir")")"
    mkdir -p "$BUILD_DIR/.skills-dist/$name"
    cp -r "$dir" "$BUILD_DIR/.skills-dist/$name/"
  done
  echo "[build-fast] skills staged: $(ls "$BUILD_DIR/.skills-dist" | wc -l) 个插件"

  # 基底镜像：必须是**干净**的完整运行时镜像。
  #
  # ⚠️ 2026-11-24: Dockerfile.patch 原来写 `FROM ru41:local` —— 每次构建都叠在
  # 上一版自己身上, 镜像 2.71 GB → 12.2 GB, 推送要 25 分钟。
  # 现在 patch 统一 FROM `ru41:base`, 这里负责把它准备好:
  # 从 CI 拉的完整镜像(ghcr)取一份 tag 成 ru41:base —— 那是**主 Dockerfile** 的产物,
  # 干净且完整(node + apt 工具 + codex/pi + /usr/share/ru41 目录结构)。
  #
  # 只在缺失时拉: 拉一次就有本地层缓存, 后面的构建直接复用。
  # 想刷新基底(拿 CI 的新基础层)就删掉 ru41:base 再构建, 或设 RU41_REFRESH_BASE=1。
  BASE_REF="${RU41_BASE_REF:-ghcr.io/windy664/ru41:latest}"
  if [[ "${RU41_REFRESH_BASE:-0}" == "1" ]] || ! docker image inspect ru41:base >/dev/null 2>&1; then
    echo "[build-fast] 准备基底镜像 ru41:base (来自 $BASE_REF)"
    docker pull "$BASE_REF"
    docker tag "$BASE_REF" ru41:base
  fi
  echo "[build-fast] 基底 ru41:base = $(docker images ru41:base --format '{{.ID}} {{.Size}}')"

docker build -f "$BUILD_DIR/deploy/Dockerfile.patch" -t ru41:local "$BUILD_DIR"

# 工具校验：基底 ru41:base 会随 CI 变(← ghcr latest)，清单可能漂移。
# 少了哪个工具，agent 抓数据/跑脚本会在**运行时**才炸 —— 那时已经上线了。
# 所以构建后立刻逐个查，缺就直接失败（别把坏镜像推上去）。
echo "[build-fast] 校验镜像里的工具"
missing=""
for tool in python3 pip3 curl jq rg zip unzip ffmpeg pandoc convert soffice sqlite3 git; do
  docker run --rm --entrypoint sh ru41:local -c "command -v $tool" >/dev/null 2>&1 \
    || missing="$missing $tool"
done
for mod in requests bs4 lxml; do
  docker run --rm --entrypoint python3 ru41:local -c "import $mod" >/dev/null 2>&1 \
    || missing="$missing py:$mod"
done
if [[ -n "$missing" ]]; then
  echo "[build-fast] FATAL: 镜像里缺这些工具:$missing"
  echo "[build-fast] 基底可能漂移了(ru41:base ← ghcr latest)，"
  echo "[build-fast] 查 Dockerfile.patch 的 apt 清单，把缺的补回去。"
  exit 1
fi
echo "[build-fast] 工具齐全"

if [[ "${PUSH:-0}" == "1" ]]; then
  python3 "$HOME/bin/ru41-push-image.py"
  # skills 随镜像外挂在服务器 /etc/ru41/modules（bind mount 自 ~/ru41-mimo-smoke/modules），
  # 不随 Dockerfile.patch 更新，这里显式同步并重启 worker 让新 skill 生效。
  TARBALL="$(mktemp /tmp/ru41-modules-XXXXXX.tgz)"
  # 技能包分布在 src/modules（内建）与 plugins（插件）两处，staging 成
  # <模块>/skills/<技能> 布局后打包。
  STAGE="$(mktemp -d)"
  cp -r "$BUILD_DIR/src/modules/." "$STAGE/"
  for d in "$BUILD_DIR"/plugins/*/skills; do
    [ -d "$d" ] || continue
    m="$(basename "$(dirname "$d")")"
    mkdir -p "$STAGE/$m"
    cp -r "$d" "$STAGE/$m/skills"
  done
  tar czf "$TARBALL" -C "$STAGE" .
  rm -rf "$STAGE"
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
