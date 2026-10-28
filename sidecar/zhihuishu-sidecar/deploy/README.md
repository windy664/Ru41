# 部署: 智慧树作业提醒闭环

两个角色可在同一台机器, 也可分离; 唯一耦合是 `report.json` 一份文件。

```
[zhihuishu-collector.timer]  每晚 23:30
        │ docker run collector.py
        ▼
zhihuishu-sidecar/data/report.json   ←契约文件（见 ../../../docs/sources-zhihuishu.md）
        │ Ru41 [sources.zhihuishu] report_path 指向它
        ▼
[Ru41] zhihuishu poll / schedule run zhihuishu  →  [notify] 出口提醒
```

## 采集端安装(任一主机)

```bash
# 1. 代码与镜像
git clone <sidecar-repo> ~/Project/zhihuishu-sidecar && cd ~/Project/zhihuishu-sidecar
docker compose build            # 或从别处 docker save|ssh|docker load 传 zhihuishu-sidecar:local

# 2. 账密(gitignored, 绝不入库)
cp config.ini.example config.ini && vi config.ini   # username/password

# 3. 手动验证一轮(产物 data/report.json)
bash -c 'Xvfb=:0 直接跑 README 里的 docker run 模板'

# 4. systemd 用户级定时
mkdir -p ~/.config/systemd/user
cp deploy/zhihuishu-collector.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now zhihuishu-collector.timer
systemctl --user start  zhihuishu-collector.service   # 立即试跑一次
journalctl --user -u zhihuishu-collector.service -n 50
```

## 消费端(Ru41 主机)配置

`config.toml` 增补:

```toml
[sources.zhihuishu]
report_path = "/home/<user>/Project/zhihuishu-sidecar/data/report.json"
# deadline_days = 3        # 缺省 3 天进入每日提醒
# notify = true            # 经 [notify] 默认出口推送
```

Ru41 侧无需额外定时器: `schedule run zhihuishu`(或常驻网关进程)按 6h 周期消费报告;
`ru41 schedule list` 应显示 `zhihuishu enabled=true`。

## 跨主机变体(采集与 Ru41 分机)

采集主机加装推送单元(采集成功后 rsync 到 Ru41 主机):

```ini
# ~/.config/systemd/user/zhihuishu-report-push.service
[Service]
Type=oneshot
ExecStart=/usr/bin/rsync -t %h/Project/zhihuishu-sidecar/data/report.json \
  <ru41-host>:Project/zhihuishu-sidecar/data/report.json
```

timer 的 `OnUnitActiveSec` 不需要——在 collector.service 里加
`OnSuccess=zhihuishu-report-push.service` 即可串联(需 systemd ≥ 249 支持 OnSuccess=)。
推荐配置 SSH key 免密; 传输只是普通用户文件, 不含账密。

## 课堂签到值守（捕获期）

工作日白天每 30 分钟一轮（25 分钟会话轮转巡课），监听课堂域 API 流量落盘
`data/signin_traffic.jsonl`，用于捕获老师发起签到时的真实端点。拿到端点实现
监测推送后本值守退役。

```sh
# 首次安装（.223 上执行）
rsync scripts/signin_watch.py deploy/ windy2@192.168.1.223:Project/zhihuishu-sidecar/ -r
ssh windy2@192.168.1.223
cp Project/zhihuishu-sidecar/deploy/zhihuishu-signin-watch.{service,timer} ~/.config/systemd/user/
systemctl --user daemon-reload && systemctl --user enable --now zhihuishu-signin-watch.timer

# 查看捕获
cat Project/zhihuishu-sidecar/data/signin_traffic.jsonl
journalctl --user -u zhihuishu-signin-watch -n 20
```

注意：用户级 timer 需 `sudo loginctl enable-linger windy2` 才能在未登录时触发。
