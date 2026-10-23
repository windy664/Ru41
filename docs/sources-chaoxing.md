# 学习通签到监控说明

学习通（超星）课程签到活动的检测与提醒。与智慧树不同，学习通协议是活的纯 HTTP
（无浏览器、无验证码路径），因此**不经过 sidecar**，协议端直连实现在 Rust 模块内，
形态与 ecard 一致。协议调研见 docs/chaoxing-recon.md。

## 架构分工

- `modules/chaoxing/source.rs`：协议客户端——`fanyalogin` 登录（AES-128-CBC 加密账密）、
  `courselistdata` 课程列表（HTML 解析）、`activelist` 签到活动（JSON）、`preSign` + `stuSignajax` 手动签到。
- `modules/chaoxing/service.rs`：轮询编排——新活动落 records 去重，待签活动经默认通知出口提醒。
- 凭证在 `[sources.chaoxing]`（建议环境变量 `RU41_XXT_PHONE` / `RU41_XXT_PASSWORD` 注入）；
  会话 cookie 持久化在 `.ru41/chaoxing/cookies.json`（gitignored），复用避免反复账密登录。

## 配置（ru41.toml）

```toml
[sources.chaoxing]
# phone = "13800000000"        # 或环境变量 RU41_XXT_PHONE
# password = "..."             # 或环境变量 RU41_XXT_PASSWORD
# cookie_path = ".ru41/chaoxing/cookies.json"
# interval_secs = 60           # 轮询间隔；签到窗口常为 1-2 分钟，太疏会漏
# notify = true                # 待签活动经 [notify] 默认出口推送
```

## 运行

```bash
ru41 chaoxing poll                          # 采集一轮并按需提醒（cron 友好）
ru41 chaoxing run                           # 常驻轮询（schedule run chaoxing 等价）
ru41 chaoxing sign 1000174332257            # 手动签普通/手势/签到码
ru41 chaoxing sign 1000174332257            # 位置签到（坐标缺省用 default_lat/lon/addr 配置）
ru41 chaoxing sign 1000174332257 --lat 30.1 --lon 120.2 --addr "教学楼"   # 显式覆盖位置
ru41 chaoxing sign 1000174332257 --enc "XXXX"        # 二维码签到（enc 来自现场二维码）
ru41 chaoxing sign 1000174332257 --photo ~/me.jpg    # 拍照签到（缺省用 photo_path 配置）
```

## 作业监控（chaoxing-work，独立低频任务）

纯 Rust 直采（无 sidecar），与签到共用登录会话和 cookie：

```
stucoursemiddle（进课）→ 提取 enc/openc/workEnc 三件套
  → mooc2/work/list（enc 必须用 workEnc，t/stuenc 伴随）
  → 未交作业才拉详情页 work/task 取截止时间（请求预算控制）
```

- 轮询间隔默认 1800s（`work_poll_secs`），与签到 60s 是两个独立调度任务
- records kind `chaoxing_work`（key `{courseId}#{workId}`）快照去重；
  临期/过期每日最多提醒一次（kind `chaoxing_work_alert`）
- 新作业提醒一次；未交→已交变迁 → ✅ 回执并自动回勾待办（`work_todo_sync`，默认开）
- **Obsidian 聚合**（`work_vault_sync`，默认开）：每课写 `学习通作业.md` 课程索引 +
  `{截止日}_{作业名}/作业说明.md`（结构与智慧树同构），并聚合两平台快照写学期级
  `作业总表.md`；`vault_course_dir` 缺省与 zhihuishu 相同学期目录，`vault_courses`
  映射跨平台同名课程到同一目录。后续新作业平台按同一结构接入。
- 红线：作业只提醒/跟踪/归档，不做自动作答（自动答题=代写）
- QQ 查询：`/作业` 是跨平台聚合命令（`modules/homework`），学习通 records 快照 +
  智慧树 sidecar 报告合并出一张清单；注册顺序保证它先于单平台 handler 命中。

```bash
ru41 chaoxing work     # 采集一轮作业并按需提醒/同步（cron 友好）
```

## 数据模型与幂等键

- records kind `chaoxing_sign`，key `{courseId}#{activityId}`：活动快照
  （课程、类型、起止时间、状态）。首次发现入库；此后只在状态变迁时经乐观并发
  （revision CAS）更新快照并产出回执，天然去重。
- records kind `chaoxing_alert`，key `login:{date}`：登录态失效告警每日最多一次。

## 通知策略

三类消息都走 `[notify]` 默认出口，推给宿主：

1. **待签提醒**（活动首次发现且进行中未签）：
   `【学习通签到】N门课程 M 个签到活动，K 个待签 - ⏰课程｜类型｜进行中，截止 …｜id=…`
   消息带 activity_id，可直接 `chaoxing sign <id>`。
2. **结果回执**（poll 检测到状态变迁，无论从哪里签的）：
   `【学习通签到回执】 - ✅课程｜类型｜签到成功 / - ❌课程｜类型｜已结束，仍未签到`
   只认两类变迁：未签→已签（✅）、进行中→结束未签（❌）；已签后的收尾状态
   变化（(1,0)→(2,0)）不重复播报。
3. **命令行签到回执**：`chaoxing sign` 成功 → `✅ 学习通签到成功：…（平台响应：…）`；
   平台拒绝 → `❌ 学习通签到失败：…`。命令行签到会同步把快照标为已签，
   下一轮 poll 不会重复播报同一条。

## 签到类型支持

全部类型共用 `stuSignajax` 一个端点，差异只在参数填充（对齐社区长期在用的实现）：

| 类型 | 命令行支持 | 参数差异 |
| --- | --- | --- |
| 普通(0) / 手势(3) / 签到码(5) | ✅ 直接签 | `objectId=aaa`、`lat/lon=-1`；先 preSign |
| 位置(4) | ✅ 坐标缺省用配置 | `name`=姓名、`address`=地址文本、真实 `latitude/longitude`、`ifTiJiao=1` |
| 二维码(2) | ✅ 需 `--enc` | `enc`=现场二维码里的时效 token + `location` JSON 参数 |
| 拍照(1) | ✅ 照片可配置 | 先传云盘（uservalid → /upload）换 `objectId` 再签；不走 preSign |

坐标与照片的缺省来源（命令行参数 > 配置 > 内置兜底）：

- 位置/二维码：`default_lat` / `default_lon` / `default_addr`（宿舍坐标与地址文本）；
- 拍照：`photo_path` 指定的本地照片；都没配时用内置兜底照片（一张已在平台验证过的图片）。
- 二维码 enc 没有缺省——它每分钟刷新、只存在于现场二维码里，物理上必须现场获取
  （扫一眼或让同学拍给你），过期重扫即可。

## 自动签到（auto_sign）

`auto_sign = true` 时进入 agent 模式：poll 发现待签活动的**当场就签**（≤60s 延迟），
全类型除二维码外全自动：

| 类型 | 自动签行为 |
| --- | --- |
| 普通/手势/签到码 | 直接签，✅ 回执 |
| 位置 | 用默认位置（default_lat/lon/addr）自动签 |
| 拍照 | 用 photo_path 或内置照片自动签 |
| 二维码 | 物理不可自动（enc 每分钟刷新、只存在于现场）→ ⏰ 提醒等 `--enc` |

失败语义（防风控的核心设计）：

- **平台明确拒绝** → 打 `auto_attempted` 标记 + ❌ 回执（含平台原话），**绝不自动重试**
  ——对同一活动反复撞接口是最典型的机器特征；人工判断后可 `chaoxing sign <id>` 重签；
- **网络等瞬时错误** → 快照不动，下一轮 poll 自然重试（窗口常为 1-2 分钟，最多撞 1-2 次）；
- 每次自动签都有 QQ 回执可审计：✅/❌ + 平台原话。

## 边界

1. 位置签到的地址文本与教师设定的签到点距离由平台校验；坐标/地址来自宿主配置。
2. 轮询是只读枚举（accountManage 校验 + 课程列表 + 每课活动列表，每轮 3+N 个请求）；
   登录优先复用持久化 cookie，账密登录只在会话失效时发生，降频保号。
3. 凭证只存 gitignored 的本地配置或环境变量；cookie 文件等同登录态，不入库。

## 边界与限制

- 课程列表只读主列表（courseFolderId=0），课程文件夹内的课程不展开（当前账号无此需求）。
- `sign` 需要活动在课程列表中可见且未结束；已结束活动平台会拒绝。
- 页面改版风险集中在 `decode_course_list`（HTML 分块正则）——协议库用 BeautifulSoup
  做同样的事，改版时对照 docs/chaoxing-recon.md 里存档的响应样本修这一个函数。
- 登录若触发平台风控（验证码），模块只能报错提醒，需人工手机端登录后重试。
