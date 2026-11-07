# 宿舍服务器掉电：排查记录与遗留问题

本文记录 2026-11-07 宿舍服务器（`windy2`，VivoBook S13，192.168.1.223）三次无预警断电的排查过程，以及过程中发现的、属于代码侧的问题。

## 现象

| 时间 | 表现 |
|---|---|
| 11-07 19:17:54 | 整机断电，内核日志戛然而止 |
| 11-07 20:54:55 | 整机断电，内核日志戛然而止 |
| 11-07 21:09:38 | 整机断电（这次是开机 8 分钟后又死了一次，`journalctl --list-boots` 才看得出来） |

三次都没有关机序列、没有 OOM、没有 kernel panic、没有 soft lockup、没有 thermal 事件、也没有电源键记录。

两个附带线索：

- **时钟会复位**。每次掉电再启动，机器的时钟都停在 `Tue 2026-07-28 04:44:02`（BIOS 给的兜底时间），要等 chrony/NTP 校回来。`journalctl --list-boots` 里能看到这个跳变，别把它当成记录错乱。
- **死前整机停顿**。20:52:15 和 20:54:55 各有一条 `perf: interrupt took too long (2538 > 2500)` / `(3185 > 3172)`——NMI 看门狗在报整机秒级卡死。

## 根因：没有电池，扛不住满载的峰值电流

机器上 `BAT0` 的 `energy_now` 读不出来（`No such device`），`status` 恒为 `Not charging`，也就是**这台机器没有可用电池，纯适配器供电**。

于是链条是：

```
某个任务把 CPU 拉满
  → 整机电流尖峰上跳
    → 适配器/主板供电保护跌落（这台机器没有任何缓冲）
      → 整机瞬间断电，内核连一条日志都来不及落盘
```

这解释了三个现象：日志戛然而止、关机序列缺失、以及"每次 CPU 一满载就关机"的规律。

## 关于 sar 的那个坑

一开始用 `sar`（10 分钟粒度）看断电前 10 分钟：

```
19时10分03秒  all  0.56  0.00  0.42  0.11  0.00  98.92
19时20分01秒  all  0.38  0.00  0.30  0.06  0.00  99.26
20时50分14秒  all  0.34  0.00  0.27  0.07  0.00  99.32
```

字段依次为 `%user %nice %system %iowait %steal %idle`。空闲率 98.8%~99.3%，看起来"根本没过载"——**这个结论是错的**：8 核机器上哪怕全核满载 5 秒，摊到 600 秒的窗口里也只有 0.7% 的 user，均值根本压不出来。10 分钟粒度的 sar 回答不了"是不是被几秒钟的尖峰打死的"这个问题。

真正的判据是死前的秒级现象：`perf: interrupt took too long` 说明整机已经卡住 2.5~3.2 秒，这才是满载 + 供电跌落的现场。

## git 这条线：查过了，不是它

早期怀疑是 git 的后台自动维护（`gc --auto` / `git maintenance run --auto`）在 merge 之后 detach 出 repack，打满多核 CPU。现象上说得通（pack 文件被重写、容器里 `[git] <defunct>` 只增不减），但用 git 自己的 trace2 记账复核之后站不住：

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

整轮里**没有出现 `gc`、`repack`、`pack-objects`、`index-pack`、`unpack-objects`、`maintenance` 中的任何一个**，全部子命令加起来不到 2 秒。同一个周期上 sar 显示整机 85%~87% 空闲。

所以关掉自动维护（`gc.auto=0`、`maintenance.auto=false`，见 `src/adapters/vault.rs` 的 `disable_background_maintenance`）确实消除了"僵尸进程只增不减"这个现象，但它**跟掉电没关系**，别把它当成已修好的结论往下传。

### 顺带查清的两件事

1. **35MB 的 pack 文件会被原地重写**（内容逐字节相同，`.idx`/`.rev` 不动）。出现在 fetch 收到对象的那几秒里，属于把同一份 pack 再写一遍，代价是一次 35MB 写盘，不是 CPU 尖峰。具体是哪个子命令写的没定位到——trace 打开之后它没再复现。
2. **松散对象只增不减**。关掉自动 gc 之后，1512 个松散对象 / 22MB 就一直躺在 `.git/objects/` 里，每轮 merge 再加几个。靠每天 04:30 的 `ru41-vault-gc.timer`（`git gc --quiet --prune=now`）收口。

## 已经做的加固

### 1. 给容器加 CPU 上限（不让任何单个任务把 8 核拉满）

```
paraformer-asr   --cpus 3     # CPU 推理，最容易一把拉满
ru41-browser     --cpus 2     # Chromium 冷启动
ru41-mimo-worker --cpus 2
ru41-obsidian    --cpus 1
ru41-lecture     --cpus 1
```

`deploy/roll.sh` 原来重建容器时只搬 `--restart`/`--env`/`-v` 这些字段，**不搬 `--cpus`**——下一次滚动镜像这些上限就没了。已在 jq 参数拼装里补上 `NanoCpus` 与 `Memory` 两项，重建后上限仍在。

### 2. 1 秒粒度的取证采样器

服务器上 `~/bin/ru41-power-watch.sh` + `~/.config/systemd/user/ru41-power-*.service`，每 1 秒写一行到 `~/.local/state/ru41-power/samples-<boot_id>.csv`：

```
23:00:30;cpu=2/2/97/0;load=0.21/0.36/0.48;ac=1;throttle=10/0
```

- `cpu` 是 `/proc/stat` 的 user/system/idle/iowait 百分比，**1 秒粒度就是为了抓住几秒级的尖峰**（sar 的教训）；
- `throttle` 是 `thermal_throttle/package_throttle_count`：CPU 触到功耗墙或温度墙时自增，这是"供电/散热已经顶不住"的直接信号；
- `ac` 来自 `/sys/class/power_supply/AC0/online`，适配器一掉就是 0；
- 每 5 秒补一行明细（温度、频率、可用内存、swap、僵尸数、CPU 前三名进程）；
- 每次开机一个独立文件，掉电前的最后几行永远留在旧文件里。

正常关机时 systemd 的 `ExecStop` 写 `clean-shutdown` 标记；下次开机若没有标记，说明上次是非正常关机，`boot-check` 会把上一次会话最后 20 行追加进 `~/.local/state/ru41-power/incidents.log`。

开机自检**不推消息**：断电是自己按电源键开的，推个"你掉电了"没有意义，有意义的是留下"死的时候到底在跑什么"。

## 代码侧还留着的真问题

`vault.sync()` 里的 `git pull --rebase origin master` **每轮都失败**：

```
error: cannot pull with rebase: You have unstaged changes.
```

原因是自提交（self-commit）跑完之后、其它插件还会往 vault 写文件，工作区不干净，rebase 直接拒绝。插件随后走 `merge_remote_pending()` 那条路（fetch → `merge --no-ff` → push → fetch → `push --delete`），于是每轮白白多一次 fetch、一次失败 rebase、一次 abort，以及一条本地 merge commit。

**已修**（`73f499f`）：`GitVault::sync()` 在拉远端之前先把本地改动提交掉
（`[ru41] 同步前提交`），脏工作区不再挡住 pull。集成测试
`tests/obsidian.rs::sync_commits_dirty_worktree_before_pulling` 覆盖这个场景。

> 顺带记一笔部署上的坑：插件 .so 静态链接了 `ru41` lib crate 的代码，
> **只换宿主二进制不刷新 .so，插件侧跑的还是旧逻辑**。发布链见
> [deploy/README](../deploy/README.md)。

## 代码侧做不了的部分

- **换适配器/电池**：这是根治项。`BAT0` 读不出电量，等于没有缓冲，适配器一抖整机就没；
- **上 UPS，或接带电量计的智能插座**：能记录 AC 事件，掉电时至少留个时间戳；
- **压 CPU 频率上限**（`intel_pstate.max_perf_pct`，需要 root）：降低峰值电流，等于给适配器减负。

## 相关提交

- `fc24788` fix(vault): 关掉 git 的后台自动维护，避免 merge 触发 repack 尖峰
