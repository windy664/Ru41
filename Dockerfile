FROM rust:1.98-trixie AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release --bin ru41

FROM node:22-trixie-slim
ARG CODEX_VERSION=0.163.0
# 容器出国际方向不稳定（宿主机代理不覆盖 docker 桥接），apt 统一走阿里云镜像。
RUN sed -i 's|deb.debian.org|mirrors.aliyun.com|g; s|security.debian.org|mirrors.aliyun.com|g' \
        /etc/apt/sources.list.d/debian.sources 2>/dev/null \
    || sed -i 's|deb.debian.org|mirrors.aliyun.com|g; s|security.debian.org|mirrors.aliyun.com|g' \
        /etc/apt/sources.list
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates sqlite3 git \
        python3 python3-pip curl jq \
        ripgrep zip unzip ffmpeg pandoc imagemagick \
    && rm -rf /var/lib/apt/lists/* \
    && pip3 install --break-system-packages --no-cache-dir requests beautifulsoup4 lxml yt-dlp \
    && npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force
RUN mkdir -p /data/codex /etc/ru41 \
    && chown -R node:node /data /etc/ru41 /home/node \
    && ln -snf /usr/share/zoneinfo/Asia/Shanghai /etc/localtime \
    && echo "Asia/Shanghai" > /etc/timezone
COPY --from=builder /build/target/release/ru41 /usr/local/bin/ru41
COPY src/modules /etc/ru41/modules
USER node
ENV HOME=/home/node
ENV CODEX_HOME=/data/codex
# 课表日程、每日提醒、日期边界都按本地时区算；容器默认 UTC 会让所有时刻错 8 小时。
ENV TZ=Asia/Shanghai
WORKDIR /home/node
ENTRYPOINT ["/usr/local/bin/ru41"]
CMD ["--help"]
