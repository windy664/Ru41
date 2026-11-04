# 出口审计（P3+ observe-only）

## 架构

```
ru41 容器群 --HTTP(S)_PROXY--> mihomo(docker0:7890, host 网络)
                                   │ info 级连接日志（docker logs）
宿主 cron（每分钟）─ collect.py ───┘ 过滤 172.16/12 → 容器名归因
                                   → .ru41/egress/conns-YYYY-MM-DD.jsonl
ru41-egress-audit 插件容器 ── 每 6h 聚合 24h → digest-latest.md
                                   └─ 白名单外新域名 → 当日首报（QQ）
```

## 安装（宿主机一次性）

1. `deploy/egress-audit/collect.py` → `/home/windy2/egress-audit/`，可执行。
2. crontab 加一行（flock 防并发）：
   `* * * * * flock -n /tmp/egress-audit.lock python3 /home/windy2/egress-audit/collect.py >> /home/windy2/egress-audit/collect.log 2>&1`
3. mihomo `log-level: info`（连接级日志是采集数据源；已持久化进 config.yaml）。
4. `bash deploy/egress-audit/inject-proxy.sh`（仅存量容器需要，注入代理 env）。

## 边界

- 只观察不阻断：内容是 TLS 加密的，本层只看「谁连了哪个域名」；
  内容级审计在应用层（P2 出站审计）。
- 覆盖依赖代理 env：容器内不走代理栈的裸 socket 流量采集不到
  （TUN 未接管容器网段）——已知残留风险。
- 日志 30 天滚动清理（collect.py 内 KEEP_DAYS）。
