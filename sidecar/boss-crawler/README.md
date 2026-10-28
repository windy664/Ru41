# boss-crawler

BOSS 直聘实习岗爬取 sidecar——Ru41 `plugins/jobs`（岗位总览）的第二数据源。

## 为什么是独立 sidecar 而不是 Ru41 插件

BOSS 直聘反爬重：需要真实浏览器、持久化登录态、滑块验证。浏览器自动化
崩溃率高、内存大，故意隔离在 Ru41 进程外——它挂了不影响 Ru41 的 22 个容器。
两侧只用文件解耦：本服务写 `boss_jobs.json`，Ru41 jobs 插件读。

## 工作原理

DrissionPage 驱动持久化 profile 的 Chromium，按「关键词 × 城市」搜索：

- 列表页截获 `zpgeek/search/joblist.json` 响应
- 详情页截获 `wapi/zpgeek/job/detail.json` 响应
- 产出 `boss_jobs.json`（跨轮合并保留历史岗位，供 Ru41 扩窗口）

## 文件

| 文件 | 用途 |
|---|---|
| `main.py` | 主爬虫（每日定时跑） |
| `login.py` | 登录态建立/检查（持久化 profile） |
| `sms*.py` | 短信验证码登录流程（手机号经 `BOSS_PHONE` 环境变量注入） |
| `solve*.py` | 滑块验证处理 |
| `check.py` / `probe.py` / `verify.py` / `warm.py` / `inject.py` | 调试与侦察脚本 |

## 部署

```bash
docker build -t boss-crawler:local .
# 首次登录（需要手机收验证码）：
BOSS_PHONE=<手机号> docker run --rm -it -v <data>:/data boss-crawler:local python login.py
# 日常爬取由 cron/systemd 定时拉起 main.py，产出挂到 Ru41 的 /data/boss/
```

Ru41 侧配置 `[sources.jobs] boss_path = "/data/boss/boss_jobs.json"` 即可消费。

## 合规说明

仅供个人求职信息聚合使用，低频爬取（每日两次），不绕过付费墙、不抓取
非公开数据。请控制频率，尊重目标站点的 robots 与服务条款。
