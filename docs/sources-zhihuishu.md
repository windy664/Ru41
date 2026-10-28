---
type: source-adapter
status: implemented
source: 智慧树 sidecar 报告文件（.ru41/zhihuishu/report.json）
updated: 2026-10-19
tags: [Ru41, 智慧树, 作业, deadline, source]
---

# 智慧树作业提醒说明

校园课程作业 deadline 提醒模块：读取 sidecar 采集器写出的报告文件，新任务落 records 去重，未完成任务按截止时间分类（新作业 / 临近截止 / 已过期 / 考试式），纯 Rust 规则判断（不经 Agent），经 `[notify]` 默认出口提醒。代码：`src/modules/zhihuishu/`（config/source/service）；配套调研与决策见 [智慧树侦察](zhihuishu-recon.md)。

## 架构分工（重要）

- **采集端不在 Ru41 内**：登录智慧树、枚举课程与任务、写 `report.json` 全部由独立 Docker sidecar（`~/Project/zhihuishu-sidecar/`，Playwright + Xvfb）完成。Ru41 是纯消费端，不接触智慧树网络协议，不持有账号密码。
- **视频学习自动化同样不在 Ru41 内**：Autovisor sidecar 负责；本模块未来只消费其产出。
- 契约是 `report.json` 一个文件；sidecar 挂了只影响当轮数据新鲜度，不影响 Ru41 运行。

## 报告文件契约（v1）

默认路径 `.ru41/zhihuishu/report.json`，JSON 结构（`//` 后为说明，实际文件不含）：

```jsonc
{
  "generated_at": "2026-10-19 15:30:00",     // sidecar 本地时间，可选
  "login_ok": true,                          // false 时每日提醒一次「需重新登录」
  "courses": [
    {
      "title": "职业生涯规划",                // 必填；参与幂等键
      "platform": "polymas",                 // polymas | wenda，可选
      "course_home_url": "https://…",        // 可选
      "content_published": true,             // 视频内容是否已发布（Autovisor 解锁信号）；null=无法判断
      "tasks": [
        {
          "activity_task_id": "9VDE…",       // 必填；平台任务 ID
          "title": "第五节课 课前测",         // 必填
          "kind": "assignment",              // assignment | group | exam | other，可选
          "start_at": "2026-10-23 00:00:00", // 布置时间（顶层 startTime 缺失时回退 classTaskTimeList[0]），可选
          "end_at": "2026-10-31 00:00:00",   // 截止时间，可选（无则不参与到期分类）
          "completed": false,                // 已完成则跳过所有提醒
          "total_score": 20.0,
          "exam_duration_minutes": null,     // >0 即考试式
          "video_invigilate": null,          // true 即考试式
          "late_submit": false,                // 详情页 isDelay；仅未完成且非考试式任务会被抓取
          "content": "作业描述…"               // 列表 content，详情页 requirements 更富时被覆盖
          "attachments": [                     // 详情页附件（sidecar 已下载；file 相对数据根）
            {"name": "作业要求.docx", "file": "attachments/{task_id}/作业要求.docx"}
          ]              // 列表 API 的 content 字段，空则 null；进入 Vault 作业说明
        }
      ]
    }
  ]
}
```

除各层 `title` / `activity_task_id` 外字段均可缺省；未知字段会被拒（`deny_unknown_fields`，方便契约演进时及时发现版本不匹配）。

## 配置（ru41.toml）

```toml
[notify]
channel = "feishu"

[sources.zhihuishu]
report_path = ".ru41/zhihuishu/report.json"  # 缺省即此路径
deadline_days = 3                            # 截止前 N 天进入每日提醒，默认 3
interval_secs = 21600                        # zhihuishu run 轮询间隔，默认 6 小时
notify = true                                # false 则只记 records 不推送
todo_sync = true                             # 未完成作业自动写进待办并在平台完成时回勾，默认 true
vault_sync = true                            # 作业清单/说明同步进 Obsidian 课程目录（需 [obsidian]），默认 true
# vault_course_dir = "01-Courses-课程/2026-Fall-大一上"   # 学期目录，默认此值
# vault_courses = { "职业生涯规划" = "大学生职业生涯规划" } # 覆盖内置课程名映射
```

## 运行

```bash
cargo run --locked -- zhihuishu poll   # 读一轮报告并按需提醒（适合系统 cron）
cargo run --locked -- zhihuishu run    # 常驻周期轮询
```

建议节奏：systemd timer 每晚先跑 sidecar 采集器，再由 `zhihuishu poll`（或常驻 `zhihuishu run`）消费新鲜报告。

## Obsidian 课程目录同步

`[obsidian]` 配置就绪时（`vault_sync` 默认开），每轮轮询把报告覆写进 Vault 课程目录：

```
01-Courses-课程/2026-Fall-大一上/
  大学生职业生涯规划/
    智慧树作业.md                          # 课程索引：未完成/已完成清单，任务行双链到作业说明
    2026.10.13_第三讲 小组课堂表现题（8分）/
      作业说明.md                          # frontmatter(course/task/kind/due/score/status) + 平台描述
      第三讲 小组课堂表现题.docx            # 平台附件（sidecar 自动下载，原名拷入）
    2026.09.29_随堂练习：生涯类书籍思维导图/
      作业说明.md
```

- 文件夹名 `{YYYY.MM.DD}_{作业名}`，日期取布置日（缺省截止日）；作业名中的文件系统不友好字符替换为「－」。
- 附件由 sidecar 从详情页 `requirementFileList` 等下载（登录态直连 OSS），Ru41 按原名拷入作业文件夹；缺失只跳过。
- 每轮整体覆写，内容无变化则不 commit 不播报；提交信息带 `[ru41]` 前缀，obsidian 整理游标只推进不重整。
- 课程名 → Vault 目录名经 `vault_courses` 映射（缺省内置当前学期映射），未映射课程按原名建目录。
- 只写 `ports/vault` 端口；`[obsidian]` 未配置或 `vault_sync = false` 时整段跳过。同步失败降级为一条 ⚠️ 提示，不影响任务与待办。

QQ 侧另有查询命令（装配层自动注册）：单聊发送 `/作业` 或直接问「作业还有哪些没交」，机器人给出**跨平台聚合**清单（学习通 + 智慧树，新平台随接入追加），按已过期/临近截止/稍后分组；需先 `绑定 <口令>`，群聊不展示（个人数据）。

## 通知策略与幂等键

| 事件 | 行为 | 幂等键（records kind `zhihuishu_task` / `zhihuishu_alert` / `zhihuishu_content`） |
| --- | --- | --- |
| 任务首次出现 | 提醒一次（考试式标注「需人工」） | `{课程标题}#{activity_task_id}`，任务快照 |
| 未完成且临近截止 / 已过期 | 每日最多提醒一次 | 同上 + `:{YYYY-MM-DD}` |
| 登录态失效 | 每日最多提醒一次 | `login:{YYYY-MM-DD}` |
| 课程视频内容首次发布 | 播报一次（Autovisor 解锁信号） | `content:{课程标题}` |
| 未完成作业首次出现 | 自动创建待办（`todo_item`，考试式 🔒 + High，remind_at=截止时刻） | 随任务快照 `todo_id` 落库 |
| 平台侧作业完成 | 回勾对应待办并播报一次；待办已被人工完成/取消则静默跳过 | 快照 `todo_synced` 标记 |

- 已完成（`completed: true`）的任务不触发任何提醒，只入快照。
- 待办同步是单向的（平台 → 待办）：在待办里手动勾掉不影响平台状态，作业没交就仍会触发「临近截止」提醒。
- 到期分类每轮重算（报告里如实反映），只有**通知发送**受幂等键约束。
- 通知合并为一条消息发送，目标由装配层按 `[notify]` 渠道解析（同 ecard 模式）。

## 红线（与调研文档一致）

- **考试式作业**（`exam_duration_minutes > 0` 或 `video_invigilate = true`）：只提醒人工完成，永不进入任何自动作答流程。
- 本模块只读报告文件，不向平台写回任何数据；作答草稿（未来的 skill）产出也必须经人工审核。

## 边界与限制

- 报告文件缺失或 JSON 损坏：当轮 `poll` 报错退出（cron 场景下次日自愈），不影响 records 中已有状态。
- `end_at` 按本机时区解析（服务器应为 Asia/Shanghai）；平台只提供分钟级时间。
- 任务完成状态来自 sidecar 抓取时刻；交作业后到下次采集之间仍会收到当日提醒，属预期。
- 老平台（wenda）课程任务已于 2026-10-26 补齐结构化解析（sidecar 点「作业考试」读 `examStu/NowExamList` + `homeworkUser/List`；字段名防御式映射，首个真实条目原文留档 `wenda_raw.jsonl` 校准）。
- `late_submit`（是否允许迟交）来自详情页 `isDelay` 字段（文本兜底）；**未完成且非考试式任务**才访问详情页，考试式任务红线跳过、绝不进入答题页（只读文本与网络响应，不点任何提交入口）。
- 详情抓取幂等：`attachments/{task_id}/` 已存在直接复用（删目录强制刷新）；单任务失败只降级，不破坏整轮。
- `content_published` 由 sidecar 点击「我的学习」解析「共 N 个学习资源」得出；wenda 平台与特殊版式（如图谱课）返回 null，宁缺毋滥。首轮部署时已发布的课程会各播报一次，属预期。
- QQ 自然语言拦截刻意保守：须同时含「作业/截止」类词与疑问色彩才应答，其余落 Agent；`/作业` 带参数时不拦截。

## 签到监测（预留章节，进行中）

> 状态：**等手机抓包**（2026-10-26 起）。本节先占位，端点配方抓到后回填。

**目标**：上课时段检测到课堂开放即 QQ 推送「课堂已开放，注意知到 App 签到」。
**红线**：只监测 + 推送，**绝不代签**（不伪造位置/照片、不抢签到码）。

已实测的底子：

- 网页端无签到面：课堂开放窗口内网页零签到 API/零弹窗，签到是知到 App 侧行为。
- `GET https://cloudapi.polymas.com/student-course/lesson/live/list?courseId=<id>&term=20271`
  + `authorization: <JWT>` 头（非 cookie）→ 200；`data` 数组非空 = 有开放课堂
  （载荷含 lessonId/classIds/teacherName）。配方存 sidecar `data/req_headers.json`。
- 2026-10-21 实测：国安教育开放课堂被该信号正确捕获。

路线（已定）：手机 mitm 抓知到 App → 定位签到状态端点 → 上课时段直连轮询（60s）→ 推送。
mitm 环境已就绪：`.223` `mitm-zhs.service`（0.0.0.0:8888，捕获插件只记 zhihuishu/polymas 域），
产出 `sidecar/data/app_traffic.jsonl`。

**待回填**：签到端点路径、请求签名/JWT 刷新方式、上课时段判定（课表已由 jwxt 模块提供）。
