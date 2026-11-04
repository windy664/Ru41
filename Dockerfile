# syntax=docker/dockerfile:1
FROM rust:1.98-trixie AS builder
WORKDIR /build
# 依赖层：只放清单 + 空壳源码，把全部外部依赖编进缓存层。
# 常规代码提交不碰这一层（只有 Cargo.lock / 清单 / crates 变化才重建），
# 8 分钟的全量编译降到只编 ru41 本体。ARG/ENV 不能放这层——git hash
# 每次提交都变，会把缓存层顶掉。
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates ./crates
COPY migrations ./migrations
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --locked --release --bin ru41 --features local-embed
# 真源码层：只重编 ru41 本体，依赖全部命中缓存。
COPY src ./src
ARG RU41_GIT_HASH_OVERRIDE=unknown
ENV RU41_GIT_HASH_OVERRIDE=$RU41_GIT_HASH_OVERRIDE
RUN cargo build --locked --release --bin ru41 --features local-embed

# 插件构建：与宿主同一源码树同一锁文件，握手（api 版本 + deps stamp）才一致。
# 产物打进镜像 /etc/ru41/plugins，镜像即完整交付物。
FROM rust:1.98-trixie AS plugins
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY crates ./crates
COPY migrations ./migrations
# 只拷插件清单（--parents 保持目录结构）：插件源码变化不波及依赖缓存层。
COPY --parents plugins/*/Cargo.toml ./
ENV CARGO_TARGET_DIR=/build/.plugins-target
# 依赖层：ru41 与全部插件用空壳 lib 预编译，外部依赖进缓存。
RUN set -e \
 && mkdir -p src && echo 'fn main() {}' > src/main.rs && touch src/lib.rs \
 && for d in plugins/*/; do cp Cargo.lock "$d/Cargo.lock"; mkdir -p "$d/src"; touch "$d/src/lib.rs"; done \
 && for d in plugins/*/; do (cd "$d" && cargo build --release --features plugin-entry); done
# 真源码层：完整 src + 插件源码覆盖空壳，只重编 ru41 与插件本体。
COPY src ./src
COPY plugins ./plugins
ARG RU41_GIT_HASH_OVERRIDE=unknown
ENV RU41_GIT_HASH_OVERRIDE=$RU41_GIT_HASH_OVERRIDE
RUN set -e; for d in plugins/*/; do cp Cargo.lock "$d/Cargo.lock"; done \
 # 防御空壳残留：依赖层 touch 过 src/lib.rs，若 COPY 进来的真实源码 mtime
 # 更旧，cargo 会误判「无变化」直接分发空壳 .so（2026-11-03 library 事故：
 # 该插件数日未动，全量重编时只它有此问题）。统一 touch 强制重编。
 && find plugins -name '*.rs' -exec touch {} + \
 && for d in plugins/*/; do (cd "$d" && cargo build --release --features plugin-entry); done \
 && mkdir -p /plugins-dist \
 # set -e 对 && 链中间的循环体不生效（2026-11-04 egress-audit 事故：cp 失败被
 # 吞、镜像缺插件但 CI 照绿），循环内必须显式 || exit。
 && for d in plugins/*/; do n=$(basename "$d"); \
      cp ".plugins-target/release/libru41_plugin_$(printf '%s' "$n" | tr - _).so" /plugins-dist/ || exit 9; done \
 && mkdir -p /plugins-skills \
 && for d in plugins/*/; do n=$(basename "$d"); \
      if [ -d "$d/skills" ]; then mkdir -p "/plugins-skills/$n" && cp -r "$d/skills" "/plugins-skills/$n/"; fi; done

FROM node:22-trixie-slim
# 智谱 GLM coding plan 只有 chat 兼容端点，codex ≥0.16x 移除了 wire_api="chat"
# 支持（2026-10-29 实证：0.163.0 直接拒绝启动，agent 任务全灭）。钉死 0.80.0；
# exec.rs 的 CliCaps 探测会按旧版能力条件传参，两边都兼容。
ARG CODEX_VERSION=0.80.0
# 容器出国际方向不稳定（宿主机代理不覆盖 docker 桥接），apt 统一走阿里云镜像。
RUN sed -i 's|deb.debian.org|mirrors.aliyun.com|g; s|security.debian.org|mirrors.aliyun.com|g' \
        /etc/apt/sources.list.d/debian.sources 2>/dev/null \
    || sed -i 's|deb.debian.org|mirrors.aliyun.com|g; s|security.debian.org|mirrors.aliyun.com|g' \
        /etc/apt/sources.list
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates sqlite3 git \
        python3 python3-pip curl jq \
        ripgrep zip unzip ffmpeg pandoc \
        libreoffice-impress fonts-noto-cjk \
    && pip3 install --break-system-packages --no-cache-dir requests beautifulsoup4 lxml yt-dlp openpyxl \
    && apt-get purge -y python3-pip python3-setuptools python3-wheel \
    && apt-get autoremove -y --purge \
    && rm -rf /var/lib/apt/lists/* /root/.cache \
    && npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force \
    && cd /usr/local/lib/node_modules/@openai/codex/vendor \
    && ls | grep -v '^x86_64-unknown-linux-musl$' | xargs rm -rf \
    && cd /
RUN mkdir -p /data/codex /etc/ru41 \
    && chown -R node:node /data /etc/ru41 /home/node \
    && ln -snf /usr/share/zoneinfo/Asia/Shanghai /etc/localtime \
    && echo "Asia/Shanghai" > /etc/timezone
COPY --from=builder /build/target/release/ru41 /usr/local/bin/ru41
COPY --from=plugins /plugins-dist/ /etc/ru41/plugins/
# 技能包烤进 /usr/share/ru41/（/etc/ru41 是宿主机 bind mount，会被整体遮蔽）。
COPY src/modules /usr/share/ru41/modules
COPY --from=plugins /plugins-skills/ /usr/share/ru41/plugins/
USER node
ENV HOME=/home/node
ENV CODEX_HOME=/data/codex
# 课表日程、每日提醒、日期边界都按本地时区算；容器默认 UTC 会让所有时刻错 8 小时。
ENV TZ=Asia/Shanghai
WORKDIR /home/node
ENTRYPOINT ["/usr/local/bin/ru41"]
CMD ["--help"]
