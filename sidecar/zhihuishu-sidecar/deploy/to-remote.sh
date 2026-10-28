#!/usr/bin/env bash
# 一键把智慧树采集端迁到远端主机(与 Ru41 同机闭环).
# 用法: ./deploy/to-remote.sh <user@host> [--with-image]
#   --with-image  直接传本机已构建镜像(压缩后约 1.4GB); 缺省则远端自行 compose build
# 首次执行会要一次 SSH 密码(ControlMaster 复用连接, 后续命令不再问).
set -euo pipefail

DEST="${1:?用法: to-remote.sh <user@host> [--with-image]}"
WITH_IMAGE="${2:-}"
REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SSH_OPTS=(-o ControlMaster=auto -o ControlPath=~/.ssh/cm-zhihuishu-%r@%h:%p -o ControlPersist=10m)

echo "==> 1/3 同步代码 + config.ini(账密) + cookies(SSH 加密传输)"
rsync -av -e "ssh ${SSH_OPTS[*]}" \
  --exclude 'Autovisor/' --exclude 'logs/' --exclude '.git/' \
  "$REPO_DIR/" "$DEST:Project/zhihuishu-sidecar/"

if [ "$WITH_IMAGE" = "--with-image" ]; then
  echo "==> 2/3 传输镜像 zhihuishu-sidecar:local (gzip 压缩, 视网络几分钟)"
  docker save zhihuishu-sidecar:local | gzip \
    | ssh "${SSH_OPTS[@]}" "$DEST" 'gunzip | docker load'
else
  echo "==> 2/3 跳过镜像(远端将 docker compose build, 但 Autovisor/ 源码未同步, 需先 git clone 上游)"
fi

echo "==> 3/3 远端安装定时器并试跑一轮"
ssh -t "${SSH_OPTS[@]}" "$DEST" '
  set -e
  cd ~/Project/zhihuishu-sidecar
  mkdir -p ~/.config/systemd/user
  cp deploy/zhihuishu-collector.service deploy/zhihuishu-collector.timer ~/.config/systemd/user/
  systemctl --user daemon-reload
  systemctl --user enable --now zhihuishu-collector.timer
  echo "--- 试跑一轮采集(约 3 分钟) ---"
  systemctl --user start zhihuishu-collector.service
  systemctl --user list-timers zhihuishu-collector.timer --no-pager
  head -c 300 ~/Project/zhihuishu-sidecar/data/report.json; echo
  echo "OK. 建议补一条(需要 sudo): sudo loginctl enable-linger $USER"
  echo "      —— 无登录会话(重启/注销)后用户级 timer 也能照跑"
'
echo "==> 完成. Ru41 侧 config.toml 记得加 [sources.zhihuishu], 见 deploy/README.md"
echo "    确认远端跑通后, 本机执行: systemctl --user disable --now zhihuishu-collector.timer"
