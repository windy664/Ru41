#!/usr/bin/env bash
# 把 RAPL PL1（长时功耗墙）钉回 15W 规格值，并装一个 systemd 单元每次开机恢复。
#
# 背景：2026-11-08 03:28 断电取证（~/.local/state/ru41-power/）——单 ru41 进程
# 单核 99% 持续约 5.5 分钟，温度 42→94°C，频率被热压 3400→1450MHz 后整机硬断电。
# 根因：这台 VivoBook S13（i3-8130U，15W 规格）的 PL1 被设成 25W，散热规格根本
# 压不住，持续满载就是缓慢自杀。钉回 15W 后持续满载也会先降频稳态，不再撞墙。
#
# 用法（需要 root）：sudo bash deploy/ru41-power-cap.sh
set -euo pipefail

LIMIT_UW=15000000   # 15W：i3-8130U 长时规格；想更保守可改 12000000

found=0
for c in /sys/class/powercap/intel-rapl*:0/constraint_0_power_limit_uw; do
  [ -e "$c" ] || continue
  echo "$LIMIT_UW" > "$c"
  echo "now: $c = $(cat "$c") uW"
  found=1
done
[ "$found" = "1" ] || { echo "没找到 RAPL 接口，这机器不支持？" >&2; exit 1; }

cat > /etc/systemd/system/ru41-rapl-pl1.service <<UNIT
[Unit]
Description=ru41: pin RAPL PL1 to ${LIMIT_UW}uW (prevent thermal hard-cut)
After=multi-user.target

[Service]
Type=oneshot
ExecStart=/bin/sh -c 'for c in /sys/class/powercap/intel-rapl*:0/constraint_0_power_limit_uw; do echo ${LIMIT_UW} > "\$c"; done'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT

systemctl daemon-reload
systemctl enable --now ru41-rapl-pl1.service
echo "已安装 ru41-rapl-pl1.service（开机自动钉 PL1=${LIMIT_UW}uW）"
