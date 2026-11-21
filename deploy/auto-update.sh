#!/usr/bin/env bash
# 生产机自更新：拉新镜像 → 比对差异 → 滚动重建 → 健康检查 → 不健康就回滚。
#
# 为什么需要它：CI 在 GitHub 上出镜像，生产机在家里 LAN（无公网入口），
# GitHub Actions 连不上它。以前的更新路径是「人 ssh 过去跑 pull-image.sh + roll.sh」，
# 人在外面这条路就断了。本脚本让生产机自己定时做这件事：
#
#   laptop 改代码 → push → CI 出镜像（GHCR）→ 生产机定时器拉 → 滚动 → 健康检查
#
# 依赖：CI 已经把镜像推到 ghcr.io/windy664/{ru41,zhihuishu-sidecar}；
#       国内从 NJU 镜像站拉（直连 ghcr.io 会卡住），拉不动时自动回退到 ghcr.io。
#       镜像站代理大镜像会中途断流（3GB 的 sidecar 镜像实测 "unexpected EOF"），
#       所以每个源都重试若干次（已下完的层留在本地，重试是接着下）。
#
# 用法：
#   bash deploy/auto-update.sh --dry-run   # 只拉镜像并报告差异，不重建（默认）
#   bash deploy/auto-update.sh             # 真更新（需 enable 文件，见下）
#
# 安全闸（按重要性）：
#   1. 没有 ~/ru41-mimo-smoke/.auto-update.enabled 这个文件 → 只报告不重建。
#      （先在机器上跑 --dry-run 看清楚，再 touch 它开自动更新。）
#   2. 重建后逐个容器查「在跑 + 没在重启循环」，不健康立刻用旧镜像回滚。
#   3. 旧镜像不删（roll.sh 只重建容器，不删镜像），回滚随时可用。
set -euo pipefail

HOME_DIR="${RU41_HOME:-$HOME/ru41-mimo-smoke}"
ENABLE_FILE="$HOME_DIR/.auto-update.enabled"
# NJU 镜像站（国内可达）；拉不动时回退到 ghcr.io。
MIRROR_RU41="${RU41_MIRROR:-ghcr.nju.edu.cn/windy664/ru41}"
MIRROR_SIDECAR="${RU41_SIDECAR_MIRROR:-ghcr.nju.edu.cn/windy664/zhihuishu-sidecar}"
DIRECT_RU41="ghcr.io/windy664/ru41"
DIRECT_SIDECAR="ghcr.io/windy664/zhihuishu-sidecar"
# 滚动后给容器起来的宽限时间，再判健康。
SETTLE_SECS="${RU41_AUTO_UPDATE_SETTLE:-45}"
# 拉镜像的重试次数与间隔。
# 为什么必须重试(2026-11-21 实测): NJU 镜像站代理大镜像会**中途断流**
# (sidecar 镜像 ~3GB, 报 "unexpected EOF"), 直接失败就永远升不上。
# docker 会把已下载完的层留在本地, 重试时接着下 —— 所以重试是有效的,
# 而不是从头再来。
PULL_RETRIES="${RU41_AUTO_UPDATE_PULL_RETRIES:-3}"
PULL_RETRY_WAIT="${RU41_AUTO_UPDATE_PULL_RETRY_WAIT:-20}"

DRY_RUN=1
[[ "${1:-}" == "--apply" ]] && DRY_RUN=0

log() { printf '[auto-update %s] %s\n' "$(date +%H:%M:%S)" "$*"; }

# 拉单个 ref; 失败重试 PULL_RETRIES 次(每次间隔 PULL_RETRY_WAIT 秒)。
PULL_LOG=/tmp/ru41-auto-pull.log
pull_one() {
  local ref="$1" attempt=1
  while [[ "$attempt" -le "$PULL_RETRIES" ]]; do
    if docker pull "$ref" > "$PULL_LOG" 2>&1; then
      return 0
    fi
    log "拉 $ref 第 ${attempt}/${PULL_RETRIES} 次失败: $(tail -n 1 "$PULL_LOG" | tr -d '\r' | cut -c1-120)"
    attempt=$((attempt + 1))
    if [[ "$attempt" -le "$PULL_RETRIES" ]]; then
      log "  ${PULL_RETRY_WAIT}s 后重试（已下完的层会留在本地，接着下）"
      sleep "$PULL_RETRY_WAIT"
    fi
  done
  return 1
}

# 拉镜像：先试镜像站，再试官方源；都失败就保留本地那份（不更新）。
pull_image() {
  local mirror="$1" direct="$2" local_tag="$3" id="" ok_ref=""
  for ref in "$mirror:latest" "$direct:latest"; do
    if pull_one "$ref"; then
      ok_ref="$ref"
      id="$(docker image inspect "$ref" --format '{{.Id}}')"
      log "拉到 $ref → ${id:7:12}"
      break
    fi
    log "拉不动 $ref"
  done
  if [[ -z "$id" ]]; then
    log "两个源都拉不动，保留本地镜像"
    return 1
  fi
  # 顺带把本地 tag 指过去：sidecar 的 systemd 单元引用的就是 zhihuishu-sidecar:local，
  # 一 shot 容器每次都按这个 tag 起，改 tag 就等于升级了。
  docker tag "$ok_ref" "$local_tag" 2>/dev/null || true
  echo "$id"
}

log "开始（dry-run=$DRY_RUN）"

RU41_ID="$(pull_image "$MIRROR_RU41" "$DIRECT_RU41" "ru41:local" || true)"
SIDECAR_ID="$(pull_image "$MIRROR_SIDECAR" "$DIRECT_SIDECAR" "zhihuishu-sidecar:local" || true)"

# 现在跑着的容器用的是哪个镜像？
running_image_id() {
  docker inspect "$1" --format '{{.Image}}' 2>/dev/null || true
}

need_roll=0
OLD_ID=""
for name in $(docker ps --format '{{.Names}}' | grep -E '^ru41-' || true); do
  cur="$(running_image_id "$name")"
  if [[ -n "$RU41_ID" && -n "$cur" && "$cur" != "$RU41_ID" ]]; then
    need_roll=1
    OLD_ID="$cur"
    log "$name 用的镜像与新镜像不同 → 需要重建"
  fi
done

if [[ "$need_roll" == "0" ]]; then
  log "Ru41 容器已在用最新镜像，无需重建"
  log "（sidecar 镜像已更新为本地 tag，下次跑挂机/采集时自动生效）"
  exit 0
fi

if [[ "$DRY_RUN" == "1" ]]; then
  log "dry-run：检测到新镜像 ${RU41_ID:7:12}（当前 ${OLD_ID:7:12}），不重建"
  log "确认无误后加 --apply，并 touch $ENABLE_FILE"
  exit 0
fi

if [[ ! -f "$ENABLE_FILE" ]]; then
  log "没有 $ENABLE_FILE —— 只报告不重建（自动更新未启用）"
  exit 0
fi

log "开始滚动重建（用 $RU41_ID）"
set +e
RU41_IMAGE="$RU41_ID" bash "$HOME_DIR/roll.sh" 2>&1 | tee /tmp/ru41-auto-roll.log
ROLL_RC="${PIPESTATUS[0]}"
set -e
if [[ "$ROLL_RC" != "0" ]]; then
  log "roll.sh 报错（rc=$ROLL_RC），尝试用旧镜像 ${OLD_ID:7:12} 回滚"
  RU41_IMAGE="$OLD_ID" bash "$HOME_DIR/roll.sh" || true
  exit 1
fi

log "等待 ${SETTLE_SECS}s 让容器起来"
sleep "$SETTLE_SECS"

# 健康检查：在跑 + 不在重启循环里（RestartCount 一直涨就是起不来）。
bad=0
for name in $(docker ps --format '{{.Names}}' | grep -E '^ru41-' || true); do
  running="$(docker inspect "$name" --format '{{.State.Running}}' 2>/dev/null || echo false)"
  restarts="$(docker inspect "$name" --format '{{.RestartCount}}' 2>/dev/null || echo 0)"
  if [[ "$running" != "true" ]]; then
    log "✗ $name 没在跑"
    bad=1
  elif [[ "${restarts:-0}" -gt 2 ]]; then
    log "✗ $name 重启了 ${restarts} 次（起不来）"
    bad=1
  fi
done

if [[ "$bad" == "1" ]]; then
  log "健康检查不过 → 用旧镜像 ${OLD_ID:7:12} 回滚"
  RU41_IMAGE="$OLD_ID" bash "$HOME_DIR/roll.sh" || true
  log "已回滚；详情见 /tmp/ru41-auto-roll.log"
  exit 1
fi

log "更新完成，全部容器健康"
