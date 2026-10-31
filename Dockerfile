FROM rust:1.98-trixie AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY crates ./crates
COPY migrations ./migrations
ARG RU41_GIT_HASH_OVERRIDE=unknown
ENV RU41_GIT_HASH_OVERRIDE=$RU41_GIT_HASH_OVERRIDE
RUN cargo build --locked --release --bin ru41 --features local-embed

# 插件构建：与宿主同一源码树同一锁文件，握手（api 版本 + deps stamp）才一致。
# 产物打进镜像 /etc/ru41/plugins，镜像即完整交付物。
FROM rust:1.98-trixie AS plugins
WORKDIR /build
COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
COPY crates ./crates
COPY migrations ./migrations
COPY plugins ./plugins
ARG RU41_GIT_HASH_OVERRIDE=unknown
ENV RU41_GIT_HASH_OVERRIDE=$RU41_GIT_HASH_OVERRIDE
ENV CARGO_TARGET_DIR=/build/.plugins-target
RUN set -e; for d in plugins/*/; do cp Cargo.lock "$d/Cargo.lock"; done \
 && for d in plugins/*/; do (cd "$d" && cargo build --release --features plugin-entry); done \
 && mkdir -p /plugins-dist \
 && for d in plugins/*/; do n=$(basename "$d"); cp ".plugins-target/release/libru41_plugin_$n.so" /plugins-dist/; done

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
        ripgrep zip unzip ffmpeg \
    && pip3 install --break-system-packages --no-cache-dir requests beautifulsoup4 lxml yt-dlp \
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
COPY src/modules /etc/ru41/modules
USER node
ENV HOME=/home/node
ENV CODEX_HOME=/data/codex
# 课表日程、每日提醒、日期边界都按本地时区算；容器默认 UTC 会让所有时刻错 8 小时。
ENV TZ=Asia/Shanghai
WORKDIR /home/node
ENTRYPOINT ["/usr/local/bin/ru41"]
CMD ["--help"]
