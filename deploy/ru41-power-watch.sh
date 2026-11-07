#!/usr/bin/env bash
# ru41 电源取证器
#   sample      每 1 秒采一行（CPU 总量 / 负载 / 降频计数 / AC），每 5 秒补一次明细
#   mark-clean  正常关机时写干净标记（systemd ExecStop 调）
#   boot-check  开机自检：把上次会话的收尾写进 incidents.log（不打扰人）
#
# 背景：2026-11-07 这台宿舍服务器（windy2 / VivoBook S13）三次无预警断电
#（19:17:54、20:54:55、21:09:38），日志戛然而然：无关机序列、无 OOM、无
# kernel panic、无 soft lockup、无 thermal、无电源键记录；死前出现过
# "perf: interrupt took too long" 的整机秒级停顿。
# 该机 /sys/class/power_supply/BAT0/energy_now 为 "No such device"、
# status=Not charging ⇒ 等于没有电池，纯适配器供电：CPU 一满载，峰值电流
# 上跳，适配器/主板保护就跌落，整机瞬间断电，内核连日志都来不及写。
#
# 所以这个采样器的用途：把「死之前那几秒 CPU 到底满没满、有没有降频、
# 适配器还在不在」留在盘上。1 秒粒度是为了抓住几秒级的尖峰——10 分钟均值
# 看不出来，sar 早就因此把人带偏过。
#
# 文件布局（~/.local/state/ru41-power/）：
#   samples-<boot_id 前 8 位>.csv   每次开机一个，掉电前的最后几行永远留在旧文件里
#   clean-shutdown                  正常关机标记（ExecStop 写，boot-check 消费）
#   incidents.log                   非正常关机记录

set -uo pipefail

STATE_DIR="${RU41_POWER_STATE_DIR:-$HOME/.local/state/ru41-power}"
MARK="$STATE_DIR/clean-shutdown"
HEARTBEAT="$STATE_DIR/last-heartbeat"
INCIDENTS="$STATE_DIR/incidents.log"

mkdir -p "$STATE_DIR" 2>/dev/null || { echo "[ru41-power] 无法创建 $STATE_DIR"; exit 1; }

boot_id() { cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown; }
boot_short() { boot_id | cut -c1-8; }
current_csv() { echo "$STATE_DIR/samples-$(boot_short).csv"; }

# /proc/stat 第一行是 CPU 总量：user nice system idle iowait irq softirq steal
cpu_line() {
    awk '/^cpu /{u=$2+$3; s=$4; i=$5+$6; w=$7; t=u+s+i+w;
            if (t>0) printf "%.0f/%.0f/%.0f/%.0f", 100*u/t, 100*s/t, 100*i/t, 100*w/t;
            else print "0/0/0/0"}' /proc/stat
}

# 降频计数：CPU 触到功耗/温度墙时会自增，跨重启不保留（reset-on-boot）
throttle_counts() {
    local p c out=""
    p=$(cat /sys/devices/system/cpu/cpu0/thermal_throttle/package_throttle_count 2>/dev/null || echo "")
    c=$(cat /sys/devices/system/cpu/cpu0/thermal_throttle/core_throttle_count 2>/dev/null || echo "")
    out="${p:-?}/${c:-?}"
    echo "$out"
}

sample_once() {
    local ts cpu load ac csv throttle
    ts=$(date +%H:%M:%S)
    csv=$(current_csv)
    cpu=$(cpu_line)
    idle="${cpu#*/*/}"; idle="${idle%%/*}"
    load=$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null | tr ' ' '/')
    ac=$(cat /sys/class/power_supply/AC0/online 2>/dev/null || echo "")
    throttle=$(throttle_counts)
    printf '%s;cpu=%s;load=%s;ac=%s;throttle=%s\n' "$ts" "$cpu" "$load" "$ac" "$throttle" >> "$csv"
    echo "$(date --iso-8601=seconds)" > "$HEARTBEAT"

    # 每 5 秒补一次明细（贵一点的东西放这里）
    if [ $(( $(date +%s) % 5 )) -eq 0 ]; then
        local temp freq mem_avail swap_used z top
        temp=$(cat /sys/class/hwmon/hwmon*/temp1_input 2>/dev/null | head -1)
        [ -n "$temp" ] && temp=$((temp / 1000)) || temp=""
        freq=$(awk '/cpu MHz/{s+=$4; n++} END{if(n) printf "%d", s/n; else print ""}' /proc/cpuinfo 2>/dev/null)
        mem_avail=$(awk '/^MemAvailable:/{printf "%d", $2/1024}' /proc/meminfo 2>/dev/null)
        swap_used=$(awk '/^SwapTotal:/{t=$2} /^SwapFree:/{f=$2} END{if(t) printf "%d", (t-f)/1024; else print ""}' /proc/meminfo 2>/dev/null)
        z=$(ps -eo stat= 2>/dev/null | grep -c '^Z')
        top=$(ps -eo pcpu=,comm= --sort=-pcpu 2>/dev/null | head -3 | tr -s ' ' | tr '\n' '|' | sed 's/|$//')
        # 高温或 CPU 高位时补 per-container 归因（docker stats 慢，平时不跑）：
        # 23 个容器进程名都是 ru41，宿主 ps 看不出是谁在烧。
        local hot=0 ctop=""
        [ -n "$temp" ] && [ "$temp" -ge 80 ] && hot=1
        [ "${idle:-100}" -lt 60 ] 2>/dev/null && hot=1
        if [ "$hot" = "1" ]; then
            ctop=$(timeout 3 docker stats --no-stream --format '{{.Name}}={{.CPUPerc}}' 2>/dev/null \
                | sort -t= -k2 -rn | head -3 | tr '\n' ' ')
        fi
        printf '  %s detail: temp=%s freq=%s mem_avail=%s swap_used=%s zombies=%s top=%s containers=%s\n' \
            "$ts" "$temp" "$freq" "$mem_avail" "$swap_used" "$z" "$top" "$ctop" >> "$csv"
    fi

    local lines
    lines=$(wc -l < "$csv" 2>/dev/null || echo 0)
    if [ "$lines" -gt 300000 ]; then
        tail -n 200000 "$csv" > "$csv.tmp" && mv "$csv.tmp" "$csv"
    fi
    ls -1t "$STATE_DIR"/samples-*.csv 2>/dev/null | tail -n +41 | while read -r old; do rm -f "$old"; done
}

cmd_sample() {
    while true; do
        sample_once
        sleep 1
    done
}

cmd_mark_clean() {
    date --iso-8601=seconds > "$MARK"
    echo "[ru41-power] 已写正常关机标记 $(cat "$MARK")"
}

# 开机自检：只记录，不推送。断电是自己按电源键开的，推消息没有意义；
# 有意义的是留下「死的时候到底在跑什么」。
cmd_boot_check() {
    if [ -f "$MARK" ]; then
        rm -f "$MARK"
        echo "[ru41-power] 上次是正常关机"
        return 0
    fi

    local cur prev_csv prev_boot prev_first prev_last rows gap_min
    cur=$(boot_short)
    prev_csv=$(ls -1t "$STATE_DIR"/samples-*.csv 2>/dev/null | grep -v "samples-$cur\.csv$" | head -1)
    if [ -z "$prev_csv" ]; then
        echo "[ru41-power] 没有上一次会话的采样文件，跳过"
        return 0
    fi
    prev_boot=$(head -n 1 "$prev_csv" | sed -n 's/.*;boot=\([^;]*\);.*/\1/p')
    prev_first=$(head -n 1 "$prev_csv" | cut -d';' -f1)
    prev_last=$(grep -v '^  ' "$prev_csv" | tail -n 1 | cut -d';' -f1)
    rows=$(grep -vc '^  ' "$prev_csv" 2>/dev/null || echo 0)
    gap_min=$(( ( $(date +%s) - $(date -d "$prev_last" +%s 2>/dev/null || echo $(date +%s)) ) / 60 ))
    [ "$gap_min" -lt 0 ] && gap_min=0

    {
        echo "=== $(date --iso-8601=seconds) 检测到非正常关机 ==="
        echo "上次会话最后心跳：$prev_last（距今约 $gap_min 分钟）"
        echo "上次会话覆盖：$prev_first → $prev_last（共 $rows 行采样）"
        echo "上次启动 boot：$prev_boot"
        echo "--- 掉电前最后 20 行（含明细）---"
        tail -n 20 "$prev_csv" | sed 's/^/  /'
        echo
    } >> "$INCIDENTS"

    echo "[ru41-power] 已记录到 $INCIDENTS"
    return 0
}

case "${1:-sample}" in
    sample) cmd_sample ;;
    mark-clean) cmd_mark_clean ;;
    boot-check) cmd_boot_check ;;
    *) echo "用法: $0 {sample|mark-clean|boot-check}"; exit 2 ;;
esac
