use anyhow::Result;
use ru41::{
    app::homework_work,
    app::todo::TodoService,
    domain::todo::{TodoInput, TodoItem, TodoOptions, TodoPriority, TodoSource},
    ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault},
};
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
    /// 忽略名单：`课程关键字｜作业关键字` 或纯作业关键字，见 ZhihuishuSection。
    pub ignore_tasks: Vec<String>,
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
/// - 任务首次出现 → 提醒一次（考试式任务会显式标注「需人工完成」，选做任务标注「选做」）；
/// - 未完成且临近截止 → 每日最多提醒一次；已过期 → 只提醒一次，之后只留在清单里；
/// - 登录态失效 → 每日最多提醒一次（提醒人工重新登录 sidecar）；
/// - 忽略名单（ignore_tasks）命中的任务完全跳过：不落库、不建待办、不进 vault。
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
            // 消息与待办标题里用课表短名（`26-27-1思想道德与法治` → `思想道德修养与法律基础`）；
            // store 的 key 仍用平台原名，保证跨轮稳定。
            let course_name = self.display_course(&course.title);
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
                        "- 📺 {course_name}｜视频内容已发布，可安排自动学习"
                    ));
                    content_ready.push(course.title.clone());
                }
            }
            for task in &course.tasks {
                // 忽略名单：平台误采的「假作业」（随堂练习等），完全跳过——
                // 不落 store、不建待办、不进 vault、不提醒。
                if is_ignored(&self.config.ignore_tasks, &course.title, &task.title) {
                    continue;
                }
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
                                    "- ⚠️ {course_name}｜{}｜待办创建失败: {}",
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
                } else {
                    // 既有任务的刷新语义（此前只同步 completed，字段漂移全丢）：
                    // ① 重开：已完成 → 未完成（老师打回/开放补交）→ 重建待办 + 提醒
                    // ② 截止时间漂移：老师改 ddl → 快照 + 待办 postpone + 提醒
                    // ③ 完成同步：平台已完成 → 回勾待办（一次性）
                    let doc = existing.as_ref().expect("else 分支必然存在快照");
                    let old_completed = doc
                        .value
                        .get("completed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false);
                    let old_end = doc
                        .value
                        .get("end_at")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    let new_end = task.end_at.clone();
                    let reopened = old_completed && !completed;
                    let drifted = old_end.as_deref() != new_end.as_deref();

                    if reopened {
                        let revision = doc.revision;
                        let mut snapshot = doc.value.clone();
                        snapshot["completed"] = serde_json::json!(false);
                        snapshot["todo_synced"] = serde_json::json!(false);
                        snapshot["end_at"] = serde_json::json!(new_end.clone());
                        let todo_id = if self.config.todo_sync {
                            self.create_todo(course, task, end_ts, is_exam_style, is_due_soon, now)
                                .ok()
                                .map(|item| item.id)
                        } else {
                            None
                        };
                        if let Some(id) = todo_id {
                            snapshot["todo_id"] = serde_json::json!(id);
                        }
                        self.store
                            .put_document("zhihuishu_task", &key, Some(revision), &snapshot)?;
                        lines.push(format!(
                            "- ♻️ {course_name}｜{}｜作业重新开放（打回/补交），已重建待办",
                            task.title
                        ));
                    } else if drifted {
                        let revision = doc.revision;
                        let mut snapshot = doc.value.clone();
                        snapshot["end_at"] = serde_json::json!(new_end.clone());
                        self.store
                            .put_document("zhihuishu_task", &key, Some(revision), &snapshot)?;
                        if let Some(todo_id) = doc
                            .value
                            .get("todo_id")
                            .and_then(serde_json::Value::as_str)
                        {
                            let _ = self.todos.postpone(
                                todo_id,
                                &new_end.clone().unwrap_or_default(),
                                now,
                            );
                        }
                        lines.push(format!(
                            "- 🕐 {course_name}｜{}｜截止变更：{} → {}",
                            task.title,
                            old_end.as_deref().map(fmt_due).unwrap_or("未知"),
                            new_end.as_deref().map(fmt_due).unwrap_or("未知")
                        ));
                    }

                    if completed {
                        // 平台侧已完成 → 回勾待办（一次性；todo 已非 Active 视为人工处理过）
                        let todo_id = doc
                            .value
                            .get("todo_id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned);
                        let already_synced = doc
                            .value
                            .get("todo_synced")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                        if let (Some(todo_id), false) = (todo_id, already_synced) {
                            let revision = doc.revision;
                            let mut snapshot = doc.value.clone();
                            let synced = match self.todos.complete(&todo_id, now) {
                                Ok(_) => {
                                    lines.push(format!(
                                        "- ✅ {course_name}｜{}｜平台已完成，待办已勾选",
                                        task.title
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
                }
                if completed {
                    continue;
                }

                let label = if is_optional(task) {
                    format!("{course_name}｜{}（选做）", task.title)
                } else {
                    format!("{course_name}｜{}", task.title)
                };
                if is_exam_style {
                    exam_style.push(label.clone());
                }
                if is_overdue {
                    overdue.push(format!(
                        "{label}（截止 {}）",
                        task.end_at.as_deref().map(fmt_due).unwrap_or("未知")
                    ));
                }
                if is_due_soon {
                    due_soon.push(format!(
                        "{label}（截止 {}）",
                        task.end_at.as_deref().map(fmt_due).unwrap_or("未知")
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
                    // 临期按天提醒（{key}:{date}）；过期只提醒一次（{key}:overdue），
                    // 之后它就躺在作业清单/总表里，不再每天刷存在感。
                    let nag_key = if is_overdue {
                        format!("{key}:overdue")
                    } else {
                        format!("{key}:{today}")
                    };
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
                    let icon = match reason {
                        "已过期" => "⚠️",
                        "临近截止" => "⏰",
                        "考试式·需人工" => "🔒",
                        _ => "🆕",
                    };
                    let todo_note = if todo_created { "｜已入待办" } else { "" };
                    lines.push(format!(
                        "- {icon} {label}｜{reason}｜截止 {}{todo_note}",
                        task.end_at.as_deref().map(fmt_due).unwrap_or("未知"),
                    ));
                }
            }
        }

        // Obsidian 课程目录同步：结果只进报告与日志，不进 QQ 推送（噪音）；
        // 失败才降级为一条提示，不影响任务与待办。
        let vault = match self.sync_vault(&report, now) {
            Ok((count, _)) => {
                vault_files = count;
                None
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
            let message = format!("## 📚 作业提醒（{}）\n\n{}", lines.len(), lines.join("\n"));
            let mut markup = ru41::ports::commands::ReplyMarkup::new();
            markup.push_row(vec![ru41::ports::commands::MarkupButton::new(
                "📋 作业清单",
                "/作业",
            )]);
            notifier
                .send_with_markup(&self.config.notify_target, None, &message, &markup)
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
        let mut written = 0usize;
        // 同一 vault 目录可能对应多个平台课程条目（如 `26-27-1…` 与 `26-27（1）…`
        // 是同一门课的两个开课实例），先按目录聚合任务再渲染索引，
        // 否则同目录的课程互相覆写索引、后写的盖住先写的。
        let mut groups: BTreeMap<String, Vec<&ZhihuishuTask>> = BTreeMap::new();
        for course in &report.courses {
            // 平台课名带学期后缀，先归一化成课表短名再查映射/建目录，
            // 保证与 jwxt 课表、lecture 课堂笔记落进同一目录。
            let normalized = normalize_course_name(&course.title);
            let dir = self
                .config
                .vault_courses
                .get(normalized.as_str())
                .cloned()
                .unwrap_or(normalized);
            let tasks = groups.entry(dir).or_default();
            for task in &course.tasks {
                if is_ignored(&self.config.ignore_tasks, &course.title, &task.title) {
                    continue;
                }
                tasks.push(task);
            }
        }
        for (dir, tasks) in &groups {
            let course_base = format!("{base}/{dir}");
            // 课程级索引：按目录名渲染（合并同目录的所有平台课程条目）
            vault.write_file(
                &format!("{course_base}/智慧树作业.md"),
                &render_course_note(dir, tasks, now, self.config.vault_task_dirs),
            )?;
            written += 1;
        }
        for course in &report.courses {
            let normalized = normalize_course_name(&course.title);
            let dir = self
                .config
                .vault_courses
                .get(normalized.as_str())
                .cloned()
                .unwrap_or(normalized);
            let course_base = format!("{base}/{dir}");
            // 任务级资料夹：{YYYY.MM.DD}_{作业名}/作业说明.md + 平台附件
            // frontmatter 保留平台原始课名，便于溯源。
            for task in &course.tasks {
                if is_ignored(&self.config.ignore_tasks, &course.title, &task.title) {
                    continue;
                }
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
                    &render_task_note(course, task),
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

    /// 消息/待办里展示用的课程短名：归一化后查 vault 课程映射，
    /// 把 `26-27-1思想道德与法治` 这类平台课名收敛成课表短名。
    fn display_course(&self, title: &str) -> String {
        let normalized = normalize_course_name(title);
        self.config
            .vault_courses
            .get(&normalized)
            .cloned()
            .unwrap_or(normalized)
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
        let mut title = format!("智慧树｜{}｜{}", self.display_course(&course.title), task.title);
        if is_exam_style {
            title = format!("🔒{title}");
        }
        if is_optional(task) {
            title = format!("{title}（选做）");
        }
        // 选做任务不因临期升级 High——不强制交的东西不值得紧急推送。
        let priority = if is_exam_style || (is_due_soon && !is_optional(task)) {
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
/// YAML plain scalar 安全输出：sidecar 上报的课名/平台名是外部数据，
/// 含 `: `、`#`、前导特殊字符或首尾空白时必须加引号，否则 Obsidian 属性面板
/// 直接报 frontmatter 解析错误（整条笔记「坏掉」）。
fn yaml_scalar(value: &str) -> String {
    const SPECIAL: &str = "-?:,[]{}#&*!|>'\"%@`";
    let needs_quote = value.is_empty()
        || value != value.trim()
        || value.starts_with(|c: char| SPECIAL.contains(c) || c.is_whitespace())
        || value.contains(": ")
        || value.contains(" #")
        || value.ends_with(':');
    if needs_quote {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn truncate_err(err: &anyhow::Error) -> String {
    let text = err.to_string();
    if text.chars().count() > 80 {
        format!("{}…", text.chars().take(80).collect::<String>())
    } else {
        text
    }
}

/// 渲染课程目录的作业索引笔记（Obsidian Tasks 格式，每轮整体覆写）。
/// `title` 是 vault 目录名（课表短名）；`tasks` 是聚合后的任务——
/// 同一目录可能合并了多个平台课程条目（同一门课的两个开课实例），
/// 因此不再写 platform 行（合并视图下单个平台值没有意义）。
fn render_course_note(
    title: &str,
    tasks: &[&ZhihuishuTask],
    now: i64,
    dirs: ru41::ports::vault::VaultTaskDirs,
) -> String {
    let mut open: Vec<&ZhihuishuTask> = Vec::new();
    let mut done: Vec<&ZhihuishuTask> = Vec::new();
    for task in tasks {
        if task.completed.unwrap_or(false) {
            done.push(*task);
        } else {
            open.push(*task);
        }
    }
    // 平台时间串 YYYY-MM-DD HH:MM:SS 字典序即时间序
    open.sort_by(|a, b| a.end_at.cmp(&b.end_at));
    done.sort_by(|a, b| b.end_at.cmp(&a.end_at));

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {}\n", yaml_scalar(title)));
    // 不写 updated 时间戳：内容须随数据变化而变化，每轮 poll 都变的时间戳
    // 会让 vault 每轮都产生 diff → commit → 触发「已同步」通知刷屏。
    out.push_str("tags:\n  - 课程\n  - 智慧树\n---\n\n");
    out.push_str(&format!("# {title} · 智慧树作业\n\n"));
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

/// 忽略名单命中判断：条目含「｜」时按 `课程关键字｜作业关键字` 双侧子串匹配，
/// 否则只匹配作业标题（任意课程下同名即忽略）。
fn is_ignored(rules: &[String], course: &str, task: &str) -> bool {
    rules.iter().any(|rule| {
        let rule = rule.trim();
        if rule.is_empty() {
            return false;
        }
        if let Some((c, t)) = rule.split_once(['｜', '|']) {
            course.contains(c.trim()) && task.contains(t.trim())
        } else {
            task.contains(rule)
        }
    })
}

/// 选做/弹性任务识别：平台没有专门字段，只能从文本判断。宁缺毋滥，
/// 只标高置信信号——明确的选做关键词，或「基础/进阶/拓展」分层结构
/// （课堂弹性练习的典型话术，不是人人必交的硬性作业）。
fn is_optional(task: &ZhihuishuTask) -> bool {
    const HINTS: &[&str] = &[
        "选做",
        "选答",
        "自愿",
        "附加题",
        "加分题",
        "额外加分",
        "课堂表现加分",
        "不作要求",
        "不做要求",
        "学有余力",
    ];
    let text_hit = |s: &str| HINTS.iter().any(|h| s.contains(h));
    if text_hit(&task.title) {
        return true;
    }
    match task.content.as_deref() {
        Some(content) => {
            text_hit(content)
                || (content.contains("基础：")
                    && (content.contains("进阶：") || content.contains("拓展：")))
        }
        None => false,
    }
}

/// 实质作业的最小正文长度（字符）：课前测类模板话约 25-30 字，
/// 真正要动笔的作业要求通常 100 字以上，取 50 作为干净的分界线。
const SUBSTANTIVE_MIN_CONTENT_CHARS: usize = 50;

/// 实质作业：正文要求达到最小长度、有平台附件，或为小组作业——值得在 vault
/// 留档、让 agent 协助产出；课前测等纯在线客观题只有模板话，任务目录纯属噪音。
/// 小组作业单独放行：线下活动/提交视频类说明往往只有一句话，但交付物是真的。
fn is_substantive(task: &ZhihuishuTask) -> bool {
    task.kind.as_deref() == Some("group")
        || task
            .content
            .as_deref()
            .is_some_and(|c| c.trim().chars().count() >= SUBSTANTIVE_MIN_CONTENT_CHARS)
        || task.attachments.as_ref().is_some_and(|a| !a.is_empty())
}

/// 课程名归一化：剥掉学校平台挂在课名尾部的学期后缀，统一到课表短名，
/// 让课表、课堂笔记、智慧树作业落进同一个 vault 目录。
/// 例：`高等数学I(理)2026年秋冬学期` → `高等数学I(理)`；
/// `大学英语（2025-2026学年第二学期）` → `大学英语`。无后缀原样返回。
fn normalize_course_name(raw: &str) -> String {
    let original = raw.trim();
    let mut name = original;
    // 整段括号包住的学期后缀：`（2025-2026学年第二学期）`
    for (open, close) in [('（', '）'), ('(', ')')] {
        if name.ends_with(close)
            && let Some(idx) = name.rfind(open)
            && term_like(&name[idx + open.len_utf8()..name.len() - close.len_utf8()])
        {
            name = name[..idx].trim_end();
        }
    }
    // 裸后缀（以「学期」结尾）：`2026年秋冬学期` / `2025-2026学年第二学期`。
    // 注意不剥尾部括号——如 `高等数学I(理)2026年秋冬学期` 的 `)` 属于课名本身。
    if name.ends_with("学期")
        && let Some(pos) = bare_term_suffix_start(&name[..name.len() - "学期".len()])
        && !name[..pos].trim_end().is_empty()
    {
        name = name[..pos].trim_end();
    }
    name.to_string()
}

/// 识别「学期」之前的裸学期段起点，返回剥完后的课名边界。
/// 形态一 `2026年秋冬`（年份+季节）；形态二 `2025-2026学年第二`（年份对+学年+第N）。
fn bare_term_suffix_start(t: &str) -> Option<usize> {
    if let Some(pos) = trailing_run_start(t, |c| {
        c.is_ascii_digit()
            || matches!(c, '-' | '~' | '～' | '—' | '/' | '年' | '月' | '春' | '夏' | '秋' | '冬')
    })
    && t[pos..].chars().any(|c| c.is_ascii_digit())
    {
        return Some(pos);
    }
    let j = t.rfind("学年")?;
    let after = &t[j + "学年".len()..];
    if !(after.len() > "第".len()
        && after.starts_with('第')
        && after["第".len()..]
            .chars()
            .all(|c| "一二三四五六七八九十0123456789".contains(c)))
    {
        return None;
    }
    let before = &t[..j];
    let pos = trailing_run_start(before, |c| {
        c.is_ascii_digit() || matches!(c, '-' | '/' | '~' | '～' | '—' | ' ')
    })?;
    before[pos..]
        .chars()
        .any(|c| c.is_ascii_digit())
        .then_some(pos)
}

/// 尾部连续满足 pred 的字符段的起点；空段返回 None。
fn trailing_run_start(s: &str, pred: impl Fn(char) -> bool) -> Option<usize> {
    let mut start = s.len();
    for (i, c) in s.char_indices().rev() {
        if pred(c) {
            start = i;
        } else {
            break;
        }
    }
    (start < s.len()).then_some(start)
}

/// 是否是「学期/学年」描述（含数字才认，避免把普通短语当学期段）。
fn term_like(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_digit()) && (s.contains("学期") || s.contains("学年"))
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
fn render_task_note(course: &ZhihuishuCourse, task: &ZhihuishuTask) -> String {
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
    out.push_str(&format!("course: {}\n", yaml_scalar(&course.title)));
    out.push_str(&format!("task: {}\n", yaml_scalar(&task.title)));
    if let Some(kind) = &task.kind {
        out.push_str(&format!("kind: {}\n", yaml_scalar(kind)));
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
    // 同课程索引：不写 updated 时间戳，避免每轮 poll 产生无意义 diff。
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

/// 平台时间串在消息里的展示：午夜的 ` 00:00:00` 是平台占位噪音，剥成纯日期；
/// 非午夜保留到分钟（`2026-11-14 12:00`）。
fn fmt_due(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(date) = trimmed.strip_suffix(" 00:00:00") {
        return date;
    }
    trimmed.get(..16).unwrap_or(trimmed)
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
    if is_optional(task) {
        line.push_str(" 🎯选做");
    }
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
    use super::{ZhihuishuTask, is_substantive, normalize_course_name};
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
                Some(vec![crate::source::ZhihuishuAttachment {
                    name: "a.docx".into(),
                    file: "attachments/x/a.docx".into(),
                }])
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
    fn group_task_is_always_substantive() {
        let mut group = task(Some("交视频。"), 0);
        group.kind = Some("group".into());
        assert!(is_substantive(&group));
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

    #[test]
    fn course_name_strips_term_suffix() {
        // 平台长名 → 课表短名
        assert_eq!(
            normalize_course_name("高等数学I(理)2026年秋冬学期"),
            "高等数学I(理)"
        );
        // 括号整段后缀
        assert_eq!(
            normalize_course_name("大学英语（2025-2026学年第二学期）"),
            "大学英语"
        );
        assert_eq!(
            normalize_course_name("大学英语(2025-2026学年第二学期)"),
            "大学英语"
        );
        // 裸学年段
        assert_eq!(
            normalize_course_name("大学英语2025-2026学年第二学期"),
            "大学英语"
        );
        // 无后缀原样返回
        assert_eq!(normalize_course_name("军事理论"), "军事理论");
        assert_eq!(
            normalize_course_name("形势与政策-2026-2027-1"),
            "形势与政策-2026-2027-1"
        );
        assert_eq!(normalize_course_name("  思想道德与法治  "), "思想道德与法治");
        // 整个名字就是学期段时不动
        assert_eq!(normalize_course_name("2026年秋冬学期"), "2026年秋冬学期");
    }
}
