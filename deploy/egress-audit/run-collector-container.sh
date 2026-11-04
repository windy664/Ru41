#!/usr/bin/env bash
# egress-audit 聚合插件容器（observe-only）：6h 轮询本地 JSONL 出日报/告警。
# 幂等：已存在则先删再建。只读挂载，无网络需求。
set -euo pipefail
cd "$HOME/ru41-mimo-smoke"

docker rm -f ru41-egress-audit 2>/dev/null || true

docker run -d --name ru41-egress-audit \
  --restart unless-stopped \
  -v "$HOME/ru41-mimo-smoke/ru41.toml:/etc/ru41/ru41.toml:ro" \
  -v "$HOME/ru41-mimo-smoke/.ru41:/data" \
  -v "$HOME/ru41-mimo-smoke/.ru41/plugins:/etc/ru41/plugins:ro" \
  --env RU41_CONFIG=/etc/ru41/ru41.toml \
  --env RU41_DB=/data/state.db \
  --network ru41-net \
  --entrypoint /usr/local/bin/ru41 \
  ru41:local \
  --config /etc/ru41/ru41.toml --db /data/state.db schedule run egress-audit

docker network connect ru41-net ru41-egress-audit 2>/dev/null || true
echo "egress-audit container ready"
