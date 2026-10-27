use ru41::{
    app::homework_work,
    app::todo::TodoService,
    domain::todo::{TodoInput, TodoItem, TodoOptions, TodoPriority, TodoSource},
    ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault},
};
use anyhow::Result;
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use crate::source::{
    ZhihuishuCourse, ZhihuishuReport, ZhihuishuTask, load_report, parse_platform_time,
};

/// 智慧树作业提醒配置（对应配置文件的 [sources.zhihuishu]）。
#[derive(Debug, Clone)]
pub struct ZhihuishuConfig {
    /// sidecar 采集器写出的报告文件路径。
    pub report_path: PathBuf,
    /// sidecar 数据根目录（报告同级）；附件 `file` 字段相对它定位。
    pub attachments_dir: PathBuf,
    /// 截止前 N 天开始每日提醒。
    pub deadline_days: i64,
    pub interval_secs: u64,
    /// 是否把未完成作业自动写进待办并在平台完成时勾选。
    pub todo_sync: bool,
    /// 是否把作业清单同步进 Obsidian 课程目录。
    pub vault_sync: bool,
    /// Vault 内课程所在学期目录。
    pub vault_course_dir: String,
    /// 智慧树课程名 → Vault 课程目录名映射。
    pub vault_courses: BTreeMap<String, String>,
    /// 任务目录策略：实质作业才建目录，纯在线任务（课前测等）不进 vault。
    pub vault_task_dirs: ru41::ports::vault::VaultTaskDirs,
    /// 提醒投递目标（装配层按 [notify] 渠道解析）。
    pub notify_target: String,
}

/// 一轮轮询的报告。
#[derive(Debug, Serialize)]
pub struct ZhihuishuPollReport {
    pub courses: usize,
    pub tasks: usize,
    pub new_tasks: usize,
    pub due_soon: Vec<String>,
    pub overdue: Vec<String>,
    pub exam_style: Vec<String>,
    /// 本次轮询首次发现「视频内容已发布」的课程（Autovisor 解锁信号）。
    pub content_ready: Vec<String>,
    /// 本轮同步的文件数与播报行。
    pub vault_files: usize,
    pub login_ok: Option<bool>,
    pub notified: bool,
}

/// 智慧树作业提醒：读取 sidecar 报告 → 新任务落 records 去重 →
/// 未完成任务按截止时间分类（纯 Rust 规则判断，不经 Agent）→ 通知提醒。
///
/// 通知策略：
/// - 任务首次出现 → 提醒一次（考试式任务会显式标注「需人工完成」）；
/// - 未完成且临近截止/已过期 → 每日最多提醒一次；
/// - 登录态失效 → 每日最多提醒一次（提醒人工重新登录 sidecar）。
///
/// 红线：考试式任务（限时/监考）只提醒人工，不进入任何自动作答流程。
#[derive(Clone)]
pub struct ZhihuishuTracker {
    todos: TodoService,
    vault: Option<Arc<dyn Vault>>,
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    config: ZhihuishuConfig,
}

impl ZhihuishuTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: ZhihuishuConfig,
        todo_options: TodoOptions,
        vault: Option<Arc<dyn Vault>>,
    ) -> Self {
        Self {
            todos: TodoService::new(store.clone(), todo_options),
            vault,
            store,
            notifier,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<ZhihuishuPollReport> {
        let report = load_report(&self.config.report_path)?;
        let now = ru41::util::now();
        let today = ru41::util::today_utc(now);
        let urgent_window = self.config.deadline_days * 86_400;

        let mut tasks = 0usize;
        let mut new_tasks = 0usize;
        let mut due_soon = Vec::new();
        let mut overdue = Vec::new();
        let mut exam_style = Vec::new();
        let mut content_ready = Vec::new();
        let mut vault_files = 0usize;
        let mut lines: Vec<String> = Vec::new();

        if report.login_ok == Some(false) {
            let key = format!("login:{today}");
            let nagged = self.store.document("zhihuishu_alert", &key)?.is_some();
            if !nagged {
                lines.push("⚠️ 智慧树登录态失效，需要在 sidecar 里重新登录（账密或扫码）".into());
                if self.notifier.is_some() {
                    self.store.put_document(
                        "zhihuishu_alert",
                        &key,
                        None,
                        &serde_json::json!({ "ts": now }),
                    )?;
                }
            }
        }

        for course in &report.courses {
            // 视频内容上线检测（Autovisor 解锁信号）：每课程只播报一次
            if course.content_published == Some(true) {
                let ckey = format!("content:{}", course.title);
                if self.store.document("zhihuishu_content", &ckey)?.is_none() {
                    self.store.put_document(
                        "zhihuishu_content",
                        &ckey,
                        None,
                        &serde_json::json!({ "ts": now }),
                    )?;
                    lines.push(format!(
                        "- 📺{}｜视频内容已发布，可安排自动学习",
                        course.title
                    ));
                    content_ready.push(course.title.clone());
                }
            }
            for task in &course.tasks {
                tasks += 1;
                let key = format!("{}#{}", course.title, task.activity_task_id);
                let is_exam_style = task.exam_duration_minutes.unwrap_or(0) > 0
                    || task.video_invigilate == Some(true);
                let completed = task.completed.unwrap_or(false);
                let end_ts = task.end_at.as_deref().and_then(parse_platform_time);
                let is_overdue = end_ts.map(|end| end < now).unwrap_or(false);
                let is_due_soon = end_ts
                    .map(|end| !is_overdue && end - now <= urgent_window)
                    .unwrap_or(false);
                let mut snapshot = serde_json::json!({
                    "course": course.title,
                    "title": task.title,
                    "kind": task.kind,
                    "end_at": task.end_at,
                    "completed": completed,
                    "exam_style": is_exam_style,
                    "total_score": task.total_score,
                    "first_seen": now,
                });
                let existing = self.store.document("zhihuishu_task", &key)?;
                let first_seen = existing.is_none();
                let mut todo_created = false;
                if first_seen {
                    // 未完成作业自动进待办（todo_sync=false 或已完成历史任务除外）
                    let todo_id = if self.config.todo_sync && !completed {
                        match self.create_todo(
                            course,
                            task,
                            end_ts,
                            is_exam_style,
                            is_due_soon,
                            now,
                        ) {
                            Ok(item) => {
                                todo_created = true;
                                Some(item.id)
                            }
                            Err(err) => {
                                lines.push(format!(
                                    "- ⚠️{}｜{}｜待办创建失败: {}",
                                    course.title,
                                    task.title,
                                    truncate_err(&err)
                                ));
                                None
                            }
                        }
                    } else {
                        None
                    };
                    if let Some(id) = todo_id {
                        snapshot["todo_id"] = serde_json::json!(id);
                    }
                    self.store
                        .put_document("zhihuishu_task", &key, None, &snapshot)?;
                    new_tasks += 1;
                } else if completed {
                    // 平台侧已完成 → 回勾待办（一次性；todo 已非 Active 视为人工处理过）
                    let todo_id = existing
                        .as_ref()
                        .and_then(|doc| doc.value.get("todo_id"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    let already_synced = existing
                        .as_ref()
                        .and_then(|doc| doc.value.get("todo_synced"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    if let (Some(todo_id), false) = (todo_id, already_synced) {
                        let doc = existing.unwrap();
                        let revision = doc.revision;
                        let mut snapshot = doc.value;
                        let synced = match self.todos.complete(&todo_id, now) {
                            Ok(_) => {
                                lines.push(format!(
                                    "- ✅{}｜{}｜平台已完成，待办已勾选",
                                    course.title, task.title
                                ));
                                true
                            }
                            Err(_) => true,
                        };
                        if synced {
                            snapshot["todo_synced"] = serde_json::json!(true);
                            snapshot["completed"] = serde_json::json!(true);
                            // 文档必然已存在，必须带 revision 走 CAS——
                            // put_document(None) 是仅新建，写回会被静默丢弃，
                            // 导致作业总表永远显示未完成、每轮重复 complete。
                            self.store.put_document(
                                "zhihuishu_task",
                                &key,
                                Some(revision),
                                &snapshot,
                            )?;
                        }
                    }
                }
                if completed {
                    continue;
                }

                let label = format!("{}｜{}", course.title, task.title);
                if is_exam_style {
                    exam_style.push(label.clone());
                }
                if is_overdue {
                    overdue.push(format!(
                        "{label}（截止 {}）",
                        task.end_at.clone().unwrap_or_default()
                    ));
                }
                if is_due_soon {
                    due_soon.push(format!(
                        "{label}（截止 {}）",
                        task.end_at.clone().unwrap_or_default()
                    ));
                }

                let mut should_notify = false;
                let mut reason = "";
                if first_seen {
                    should_notify = true;
                    reason = if is_exam_style {
                        "考试式·需人工"
                    } else {
                        "新作业"
                    };
                }
                if is_due_soon || is_overdue {
                    let nag_key = format!("{key}:{today}");
                    let nagged = self.store.document("zhihuishu_alert", &nag_key)?.is_some();
                    if !nagged && self.notifier.is_some() {
                        self.store.put_document(
                            "zhihuishu_alert",
                            &nag_key,
                            None,
                            &serde_json::json!({ "ts": now }),
                        )?;
                        should_notify = true;
                        reason = if is_overdue {
                            "已过期"
                        } else {
                            "临近截止"
                        };
                    }
                }
                if should_notify {
                    let style = if is_exam_style {
                        "🔒考试式(人工) "
                    } else {
                        ""
                    };
                    let todo_note = if todo_created { "｜已入待办" } else { "" };
                    lines.push(format!(
                        "- {style}{label}｜{reason}｜截止 {}{todo_note}",
                        task.end_at.clone().unwrap_or_else(|| "未知".into()),
                    ));
                }
            }
        }

        // Obsidian 课程目录同步：失败只降级为一条提示，不影响任务与待办
        let vault = match self.sync_vault(&report, now) {
            Ok((count, line)) => {
                vault_files = count;
                line
            }
            Err(err) => Some(format!("- ⚠️ Obsidian 同步失败: {}", truncate_err(&err))),
        };
        if let Some(line) = vault {
            lines.push(line);
        }

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = &self.notifier
        {
            let message = format!(
                "【作业提醒】{}门课程 {} 个任务\n{}",
                report.courses.len(),
                tasks,
                lines.join("\n")
            );
            notifier
                .send(&self.config.notify_target, None, &message)
                .await?;
            notified = true;
        }

        Ok(ZhihuishuPollReport {
            courses: report.courses.len(),
            tasks,
            new_tasks,
            due_soon,
            overdue,
            exam_style,
            content_ready,
            vault_files,
            login_ok: report.login_ok,
            notified,
        })
    }

    /// 把每门课程的作业清单覆写进 Vault 课程目录（`<学期>/<课程>/智慧树作业.md`）。
    /// 有实际变更才 commit+push（`[ru41]` 前缀自提交，obsidian 游标只推进不整理）。
    fn sync_vault(&self, report: &ZhihuishuReport, now: i64) -> Result<(usize, Option<String>)> {
        let Some(vault) = &self.vault else {
            return Ok((0, None));
        };
        if !self.config.vault_sync {
            return Ok((0, None));
        }
        let base = self.config.vault_course_dir.trim_end_matches('/');
        let offset = self.todos.options().offset_seconds;
        let mut written = 0usize;
        for course in &report.courses {
            let dir = self
                .config
                .vault_courses
                .get(course.title.as_str())
                .cloned()
                .unwrap_or_else(|| course.title.clone());
            let course_base = format!("{base}/{dir}");
            // 课程级索引：未完成/已完成清单，任务行双链到作业文件夹
            vault.write_file(
                &format!("{course_base}/智慧树作业.md"),
                &render_course_note(course, now, offset, self.config.vault_task_dirs),
            )?;
            written += 1;
            // 任务级资料夹：{YYYY.MM.DD}_{作业名}/作业说明.md + 平台附件
            for task in &course.tasks {
                // substantive 策略：纯在线任务（无正文要求、无附件）不建目录，
                // 它们只出现在课程索引与作业总表里。
                if self.config.vault_task_dirs == ru41::ports::vault::VaultTaskDirs::Substantive
                    && !is_substantive(task)
                {
                    continue;
                }
                let folder = task_folder_name(task);
                vault.write_file(
                    &format!("{course_base}/{folder}/作业说明.md"),
                    &render_task_note(course, task, now, offset),
                )?;
                written += 1;
                written += self.copy_task_attachments(
                    task,
                    vault.as_ref(),
                    &format!("{course_base}/{folder}"),
                )?;
            }
        }
        if written == 0 {
            return Ok((0, None));
        }
        if vault.commit_all("[ru41] zhihuishu 课程作业同步")? {
            vault.push()?;
            Ok((
                written,
                Some(format!(
                    "- 📝 作业资料已同步到 Obsidian（{written} 个笔记）"
                )),
            ))
        } else {
            Ok((0, None)) // 内容无变化，不提交不播报
        }
    }

    /// 把 sidecar 已下载的附件拷进 Vault 作业文件夹；返回成功拷贝数。
    /// 单个附件缺失/路径非法只跳过，不阻塞整体同步。
    fn copy_task_attachments(
        &self,
        task: &ZhihuishuTask,
        vault: &dyn Vault,
        dest_base: &str,
    ) -> Result<usize> {
        let Some(list) = &task.attachments else {
            return Ok(0);
        };
        let mut copied = 0;
        for att in list {
            // 路径来自报告文件; 拒绝越出附件根的相对跳转
            if att.file.split(['/', '\\']).any(|seg| seg == "..") {
                continue;
            }
            let source = self.config.attachments_dir.join(&att.file);
            let Ok(data) = std::fs::read(&source) else {
                continue;
            };
            let name = homework_work::sanitize_title(&att.name);
            vault.write_bytes(&format!("{dest_base}/{name}"), &data)?;
            copied += 1;
        }
        Ok(copied)
    }

    /// 把未完成作业写进待办：考试式/临期 → High，remind_at 对齐截止时刻。
    fn create_todo(
        &self,
        course: &ZhihuishuCourse,
        task: &ZhihuishuTask,
        end_ts: Option<i64>,
        is_exam_style: bool,
        is_due_soon: bool,
        now: i64,
    ) -> Result<TodoItem> {
        let mut title = format!("智慧树｜{}｜{}", course.title, task.title);
        if is_exam_style {
            title = format!("🔒{title}");
        }
        let priority = if is_exam_style || is_due_soon {
            TodoPriority::High
        } else {
            TodoPriority::Normal
        };
        self.todos.add(
            TodoInput {
                title,
                priority,
                labels: vec!["智慧树".into()],
                due_at: end_ts,
                remind_at: end_ts,
                source: Some(TodoSource {
                    channel: "zhihuishu".into(),
                    source_id: Some(task.activity_task_id.clone()),
                }),
            },
            now,
        )
    }
}

/// 错误信息截断，避免推送消息被单条异常撑爆。
fn truncate_err(err: &anyhow::Error) -> String {
    let text = err.to_string();
    if text.chars().count() > 80 {
        format!("{}…", text.chars().take(80).collect::<String>())
    } else {
        text
    }
}

/// 渲染单门课程的作业清单笔记（Obsidian Tasks 格式，每轮整体覆写）。
fn render_course_note(
    course: &ZhihuishuCourse,
    now: i64,
    offset_seconds: i32,
    dirs: ru41::ports::vault::VaultTaskDirs,
) -> String {
    let mut open: Vec<&ZhihuishuTask> = Vec::new();
    let mut done: Vec<&ZhihuishuTask> = Vec::new();
    for task in &course.tasks {
        if task.completed.unwrap_or(false) {
            done.push(task);
        } else {
            open.push(task);
        }
    }
    // 平台时间串 YYYY-MM-DD HH:MM:SS 字典序即时间序
    open.sort_by(|a, b| a.end_at.cmp(&b.end_at));
    done.sort_by(|a, b| b.end_at.cmp(&a.end_at));

    let stamp = homework_work::local_stamp(now, offset_seconds);
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {}\n", course.title));
    if let Some(platform) = &course.platform {
        out.push_str(&format!("platform: {platform}\n"));
    }
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str("tags:\n  - 课程\n  - 智慧树\n---\n\n");
    out.push_str(&format!("# {} · 智慧树作业\n\n", course.title));
    out.push_str("> 由 Ru41 每轮轮询自动覆写，请勿手改；截止时间为平台北京时间。\n\n");

    out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
    if open.is_empty() {
        out.push_str("当前没有未完成作业 🎉\n");
    }
    for task in &open {
        out.push_str(&task_line_item(task, now, dirs));
    }

    if !done.is_empty() {
        out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
        for task in &done {
            out.push_str(&task_line_item(task, now, dirs));
        }
    }
    out
}

/// 任务索引行：建目录的任务双链到作业文件夹；substantive 策略下不建目录的
/// 纯在线任务降级为纯文本行，避免 Obsidian 里挂着一堆空链接。
fn task_line_item(
    task: &ZhihuishuTask,
    now: i64,
    dirs: ru41::ports::vault::VaultTaskDirs,
) -> String {
    let checked = if task.completed.unwrap_or(false) {
        "x"
    } else {
        " "
    };
    let link = dirs == ru41::ports::vault::VaultTaskDirs::All || is_substantive(task);
    let line = task_line(task, now);
    if link {
        format!(
            "- [{checked}] [[{}/作业说明|{line}]]\n",
            task_folder_name(task)
        )
    } else {
        format!("- [{checked}] {line}\n")
    }
}

/// 实质作业的最小正文长度（字符）：课前测类模板话约 25-30 字，
/// 真正要动笔的作业要求通常 100 字以上，取 50 作为干净的分界线。
const SUBSTANTIVE_MIN_CONTENT_CHARS: usize = 50;

/// 实质作业：正文要求达到最小长度或有平台附件——值得在 vault 留档、
/// 让 agent 协助产出；课前测等纯在线客观题只有模板话，任务目录纯属噪音。
fn is_substantive(task: &ZhihuishuTask) -> bool {
    task.content
        .as_deref()
        .is_some_and(|c| c.trim().chars().count() >= SUBSTANTIVE_MIN_CONTENT_CHARS)
        || task.attachments.as_ref().is_some_and(|a| !a.is_empty())
}

/// 任务文件夹名：`YYYY.MM.DD_作业名`（日期取布置日，缺省用截止日，都没有则只留作业名）。
/// 同时替换文件系统不友好字符。
fn task_folder_name(task: &ZhihuishuTask) -> String {
    homework_work::work_folder_name(
        &task.title,
        task.start_at.as_deref().or(task.end_at.as_deref()),
        None,
    )
}

/// 渲染单个作业的「作业说明.md」：元信息 + 平台描述文本。
fn render_task_note(
    course: &ZhihuishuCourse,
    task: &ZhihuishuTask,
    now: i64,
    offset_seconds: i32,
) -> String {
    let completed = task.completed.unwrap_or(false);
    let exam = task.exam_duration_minutes.unwrap_or(0) > 0 || task.video_invigilate == Some(true);
    let status = if exam {
        "🔒考试式，需人工完成"
    } else if completed {
        "✅ 已完成"
    } else {
        "未完成"
    };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {}\n", course.title));
    out.push_str(&format!("task: {}\n", task.title));
    if let Some(kind) = &task.kind {
        out.push_str(&format!("kind: {kind}\n"));
    }
    if let Some(end) = &task.end_at {
        out.push_str(&format!("due: \"{end}\"\n"));
    }
    if let Some(score) = task.total_score {
        out.push_str(&format!("score: {score}\n"));
    }
    out.push_str(&format!(
        "status: {}\n",
        if completed { "done" } else { "open" }
    ));
    out.push_str(&format!(
        "updated: {}\n",
        homework_work::local_stamp(now, offset_seconds)
    ));
    out.push_str("tags:\n  - 课程\n  - 智慧树\n  - 作业\n---\n\n");
    out.push_str(&format!("# {}\n\n", task.title));
    out.push_str(&format!("- 课程：{}\n", course.title));
    if let Some(end) = &task.end_at {
        out.push_str(&format!("- 截止：{end}（北京时间）\n"));
    }
    if let Some(start) = &task.start_at {
        out.push_str(&format!("- 布置：{start}\n"));
    }
    if let Some(score) = task.total_score {
        out.push_str(&format!("- 分值：{score}\n"));
    }
    out.push_str(&format!("- 状态：{status}\n"));
    out.push_str("\n## 作业说明\n\n");
    match task
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(text) => out.push_str(text),
        None => out.push_str("（列表接口未附带说明文字。）"),
    }
    out.push_str("\n\n> 由 Ru41 自智慧树 sidecar 报告自动生成与更新。\n");
    out
}

/// 单条未完成任务行：📅 日期 + 考试式/过期标注。
fn task_line(task: &ZhihuishuTask, now: i64) -> String {
    let date = task.end_at.as_deref().and_then(|s| s.get(..10));
    let exam = task.exam_duration_minutes.unwrap_or(0) > 0 || task.video_invigilate == Some(true);
    let overdue = task
        .end_at
        .as_deref()
        .and_then(parse_platform_time)
        .is_some_and(|end| end < now);
    let mut line = String::new();
    if overdue {
        line.push_str("⚠️已过期 ");
    }
    line.push_str(&task.title);
    if exam {
        let invigilated = if task.video_invigilate == Some(true) {
            "（监考）"
        } else {
            ""
        };
        line.push_str(&format!(" 🔒考试式{invigilated}，需人工完成"));
    }
    if let Some(date) = date {
        line.push_str(&format!(" 📅 {date}"));
    }
    line
}

impl ru41::ingress::schedule::PollJob for ZhihuishuTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        let owned = self.clone();
        ru41::plugins::bridge(async move { Ok(serde_json::to_value(owned.poll_once().await?)?) })
    }
}

#[cfg(test)]
mod tests {
    use super::{ZhihuishuTask, is_substantive};
    use ru41::ports::vault::VaultTaskDirs;

    fn task(content: Option<&str>, attachments: usize) -> ZhihuishuTask {
        ZhihuishuTask {
            activity_task_id: "id".into(),
            title: "课前测".into(),
            kind: None,
            start_at: None,
            end_at: None,
            completed: None,
            total_score: None,
            exam_duration_minutes: None,
            video_invigilate: None,
            late_submit: None,
            content: content.map(str::to_owned),
            attachments: if attachments > 0 {
                Some(vec![
                    crate::source::ZhihuishuAttachment {
                        name: "a.docx".into(),
                        file: "attachments/x/a.docx".into(),
                    },
                ])
            } else {
                None
            },
        }
    }

    #[test]
    fn online_quiz_is_not_substantive() {
        assert!(!is_substantive(&task(None, 0)));
        assert!(!is_substantive(&task(Some("   "), 0)));
    }

    #[test]
    fn written_work_and_attachment_are_substantive() {
        assert!(is_substantive(&task(
            Some(
                "基础：用「一切社会关系的总和」分析一件你身边的真实事 —— 不少于 300 字。要求写具体的事，不写概念复述。"
            ),
            0
        )));
        assert!(is_substantive(&task(None, 1)));
    }

    #[test]
    fn quiz_boilerplate_is_not_substantive() {
        // 课前测模板话约 25-30 字，低于最小正文长度。
        assert!(!is_substantive(&task(
            Some("设置三个习题，用于回顾第三节的知识点，请认真完成。"),
            0
        )));
    }

    #[test]
    fn task_dirs_policy_parse() {
        assert_eq!(VaultTaskDirs::parse(None).unwrap(), VaultTaskDirs::All);
        assert_eq!(
            VaultTaskDirs::parse(Some("all")).unwrap(),
            VaultTaskDirs::All
        );
        assert_eq!(
            VaultTaskDirs::parse(Some("substantive")).unwrap(),
            VaultTaskDirs::Substantive
        );
        assert!(VaultTaskDirs::parse(Some("bogus")).is_err());
    }
}
