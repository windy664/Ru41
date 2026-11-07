# 宿舍服务器掉电：排查记录与遗留问题

本文记录 2026-11-07 宿舍服务器（`windy2`，VivoBook S13，192.168.1.223）两次无预警断电的排查过程，以及过程中发现的、属于代码侧的问题。

## 现象

| 时间 | 表现 |
|---|---|
| 11-07 19:17:54 | 整机断电，内核日志戛然而止 |
| 11-07 20:54:55 | 整机断电，内核日志戛然而止 |

两次都没有关机序列、没有 OOM、没有 kernel panic、没有 soft lockup、没有 thermal 事件、也没有电源键记录；AC 恢复后机器重新启动。

## 根因：供电，不是负载

机器上 `BAT0` 的 `energy_now` 读不出来（`No such device`），`status` 恒为 `Not charging`，也就是**这台机器没有可用电池，纯适配器供电**。适配器或墙插一旦抖动，电压直接掉到 0，内核连一条日志都来不及落盘——这就是"日志戛然而止"的原因。

sar 归档（`/var/log/sysstat/sa06`，10 分钟粒度）里两次断电前的负载：

```
19时00分10秒  all  0.52  0.00  0.52  0.14  0.00  98.83
19时10分03秒  all  0.56  0.00  0.42  0.11  0.00  98.92
19时20分01秒  all  0.38  0.00  0.30  0.06  0.00  99.26     ← 19:17 断电
...
20时50分14秒  all  0.34  0.00  0.27  0.07  0.00  99.32     ← 20:54 断电
```

字段依次为 `%user %nice %system %iowait %steal %idle`。断电前 10 分钟 CPU 空闲率 98.8%~99.3%，`%user ≤ 0.6%`，`%iowait ≤ 0.14%`：既没有 CPU 打满，也没有 I/O 风暴。一次 34MB 仓库的 repack 至少会在 10 分钟窗口里留下 5% 以上的 `%user`，这里没有。

**结论：断电是供电事件，与负载无关。** 代码侧能做的只有降低负载尖峰的概率；要根治得换适配器/电池或上 UPS。

## "git 导致 CPU 过载"这个判断不成立

排查早期怀疑是 git 的后台自动维护（`gc --auto` / `git maintenance run --auto`）在 merge 之后 detach 出 repack，父子进程一断，repack 被 reparent 到 PID 1 无人回收，容器里堆到 57~60 个 `[git] <defunct>`，同时 repack 打满多核 CPU。这个链条在**现象上**说得通（pack 文件被重写、僵尸进程只增不减），但用 git 自己的 trace2 记账复核之后，站不住：

对 `ru41-obsidian` 容器里插件实际执行的 git 命令开 `GIT_TRACE2_EVENT`，一个完整轮询周期（5 分钟一次）的记账是：

```
git -c core.quotepath=false fetch origin            0.43s   （含 remote-https + 协商 rev-list）
git -c core.quotepath=false rev-parse origin/master  0.002s
git -c core.quotepath=false pull --rebase origin master
    └ git merge-base --fork-point ...                0.006s
    └ git fetch --update-head-ok origin master       0.34s
    └ git merge --ff-only --no-autostash FETCH_HEAD  失败
git -c core.quotepath=false rev-list --count ...     0.003s
git send-pack --stateless-rpc --thin ... gitee       0.62s
```

整轮里**没有出现 `gc`、`repack`、`pack-objects`、`index-pack`、`unpack-objects`、`maintenance` 中的任何一个**，全部子命令加起来不到 2 秒。同一个周期上 sar 显示整机 85%~87% 空闲，pidstat 里 git 进程进不了前 25。

所以关掉自动维护（`gc.auto=0`、`maintenance.auto=false`，见 `src/adapters/vault.rs` 的 `disable_background_maintenance`）确实消除了"僵尸进程只增不减"这个现象，但它**本来就不是掉电原因**，别把它当成已修好的结论往下传。

### 顺带查清的两件事

1. **35MB 的 pack 文件会被原地重写**（内容逐字节相同，`.idx`/`.rev` 不动）。出现在 fetch 收到对象的那几秒里，属于把同一份 pack 再写一遍，代价是一次 35MB 写盘，不是 CPU 尖峰，也没有和掉电时间对上（22:38、22:39 各重写一次，机器安然无恙）。具体是哪个子命令写的没有定位到——trace 打开之后它没再复现。
2. **松散对象只增不减**。关掉自动 gc 之后，1512 个松散对象 / 22MB 就一直躺在 `.git/objects/` 里，每轮 merge 再加几个。靠每天 04:30 的 `ru41-vault-gc.timer`（`git gc --quiet --prune=now`）收口。

## 代码侧还留着的真问题

`vault.sync()` 里的 `git pull --rebase origin master` **每轮都失败**：

```
error: cannot pull with rebase: You have unstaged changes.
```

原因是自提交（self-commit）跑完之后、其它插件还会往 vault 写文件，工作区不干净，rebase 直接拒绝。插件随后走 `merge_remote_pending()` 那条路（fetch → `merge --no-ff` → push → fetch → `push --delete`），于是每轮白白多一次 fetch、一次失败 rebase、一次 abort，以及一条本地 merge commit。

这不是 CPU 问题，但每 5 分钟一轮的固定开销里有一部分是它。想修有两个方向，都动核心（`src/adapters/vault.rs`），要跟核心开发组对齐：

- 让 sync 在 pull 之前先把工作区提交干净（等价于把 self-commit 挪到 pull 之前）；
- 或者给 pull 加 `--autostash`，让它自己处理脏工作区（会改变历史形状，从 merge 提交变成线性 rebase）。

## 留存的取证工具

服务器上装了一个 systemd 用户级采样器（`~/bin/ru41-power-watch.sh` + `~/.config/systemd/user/ru41-power-*.service`），每 5 秒写一行到 `~/.local/state/ru41-power/samples-<boot_id>.csv`：

```
时间;boot=…;ac=1;temp=48;freq=925;load=0.30/0.52/0.55;mem_total=7278;mem_avail=4788;swap_used=0;zombies=1
```

- `ac` 来自 `/sys/class/power_supply/AC0/online`，适配器一掉就是 0；
- 每次开机一个独立文件，掉电前的最后几行永远留在旧文件里；
- 正常关机时 systemd 的 `ExecStop` 写 `clean-shutdown` 标记；下次开机若没有标记，说明上次是非正常关机，`boot-check` 会把上一次会话的最后 8 次采样追加进 `~/.local/state/ru41-power/incidents.log`。

开机自检**不推消息**：断电是自己按电源键开的，推个"你掉电了"没有意义，有意义的是留下"死的时候到底在跑什么"。

## 代码侧做不了的部分

- 换适配器/电池：这块机器 `BAT0` 读不出电量，等于没有缓冲，适配器一抖整机就没；
- 上 UPS 或接带电量计的智能插座：能记录 AC 事件，掉电时至少留个时间戳。

## 相关提交

- `fc24788` fix(vault): 关掉 git 的后台自动维护，避免 merge 触发 repack 尖峰
