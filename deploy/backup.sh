#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="state-${timestamp}.db"

mkdir -p ../.ru41/backups
docker compose exec -T worker sqlite3 /data/state.db ".backup '/data/backups/${backup}'"
printf '.ru41/backups/%s\n' "$backup"
