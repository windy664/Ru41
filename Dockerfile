FROM rust:1.98-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
RUN cargo build --locked --release --bin ru41

FROM node:22-bookworm-slim
ARG CODEX_VERSION=0.163.0
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates sqlite3 git \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g "@openai/codex@${CODEX_VERSION}" \
    && npm cache clean --force
RUN mkdir -p /data/codex /etc/ru41 \
    && chown -R node:node /data /etc/ru41 /home/node
COPY --from=builder /build/target/release/ru41 /usr/local/bin/ru41
COPY src/modules /etc/ru41/modules
USER node
ENV HOME=/home/node
ENV CODEX_HOME=/data/codex
WORKDIR /home/node
ENTRYPOINT ["/usr/local/bin/ru41"]
CMD ["--help"]
