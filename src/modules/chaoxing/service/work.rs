//! 学习通作业监控：进课取 enc → 作业列表 → 未交分类 → 通知/待办/Obsidian。
//! 与智慧树作业模块同构，共享同一 Vault 学期目录并聚合作业总表。
//! 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。

use crate::app::todo::TodoService;
use crate::modules::chaoxing::config::ChaoxingConfig;
use crate::modules::chaoxing::source::{ChaoxingClient, ChaoxingWork};
use crate::ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault};
use anyhow::Result;
use chrono::TimeZone;
use serde::Serialize;
use std::sync::Arc;

/// vault 渲染缓存：(课程名, [(作业, 截止)]) 列表。
type CourseNotes = Vec<(String, Vec<(ChaoxingWork, Option<String>)>)>;

#[derive(Debug, Serialize)]
pub struct ChaoxingWorkPollReport {
    pub login_ok: bool,
    pub courses: usize,
    pub works: usize,
    /// 本轮首次发现的未交作业数。
    pub new_opens: usize,
    /// 本轮新进入临期窗口的作业数。
    pub due_soon: usize,
    /// 当前已过期未交的作业数。
    pub overdue: usize,
    /// 本轮检测到「已交」变迁的作业数（自动回勾待办）。
    pub submitted: usize,
    pub todo_created: usize,
    pub vault_files: usize,
    pub notified: bool,
}

/// 学习通作业监控：复用签到模块的协议客户端（同一 cookie 会话），低频轮询：
/// 进课取 enc 三件套 → 作业列表 → 未交作业按截止分类 → 通知/待办/Obsidian。
///
/// 与智慧树作业模块（sources.zhihuishu）同构，并共享同一个 Vault 学期目录：
/// 每门课写 `学习通作业.md` 课程索引 + `{截止日}_{作业名}/作业说明.md`，
/// 再把两个平台的快照聚合成学期级 `作业总表.md`——后续新作业平台照此接入。
///
/// 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。
/// 请求预算：每课 2 个只读请求（middle+list），详情页只对未交作业拉取；
/// 平台返回无权限/3004 时跳过该课并不刷屏（下轮重试）。
pub struct ChaoxingWorkTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: ChaoxingClient,
    todos: TodoService,
    vault: Option<Arc<dyn Vault>>,
    config: ChaoxingConfig,
}

impl ChaoxingWorkTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: ChaoxingClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: ChaoxingConfig,
        todo_options: crate::domain::todo::TodoOptions,
        vault: Option<Arc<dyn Vault>>,
    ) -> Self {
        Self {
            todos: TodoService::new(store.clone(), todo_options),
            store,
            notifier,
            client,
            vault,
            config,
        }
    }

    pub async fn poll_once(&self) -> Result<ChaoxingWorkPollReport> {
        use crate::domain::todo::{TodoInput, TodoPriority, TodoSource};
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        let offset = self.todos.options().offset_seconds;

        if let Err(err) = self.client.ensure_session().await {
            // 与签到模块共用 login:{date} 告警键：同一会话一天最多告警一次
            let key = format!("login:{today}");
            let nagged = self.store.document("chaoxing_alert", &key)?.is_some();
            let mut notified = false;
            if !nagged && let Some(notifier) = self.notifier.as_ref() {
                self.store.put_document(
                    "chaoxing_alert",
                    &key,
                    None,
                    &serde_json::json!({ "ts": now }),
                )?;
                notifier
                    .send(
                        &self.config.notify_target,
                        None,
                        &format!("⚠️ 学习通登录态失效（作业轮询）：{err}"),
                    )
                    .await?;
                notified = true;
            }
            return Ok(ChaoxingWorkPollReport {
                login_ok: false,
                courses: 0,
                works: 0,
                new_opens: 0,
                due_soon: 0,
                overdue: 0,
                submitted: 0,
                todo_created: 0,
                vault_files: 0,
                notified,
            });
        }

        let courses = self.client.course_list().await?;
        let urgent_window = self.config.work_due_days * 86_400;
        let mut works_total = 0usize;
        let mut new_opens = 0usize;
        let mut due_soon = 0usize;
        let mut overdue_total = 0usize;
        let mut submitted = 0usize;
        let mut todo_created = 0usize;
        let mut lines: Vec<String> = Vec::new();
        // vault 渲染缓存：(课程名, [(作业, 截止)])
        let mut course_notes: CourseNotes = Vec::new();

        for course in &courses {
            let encs = match self.client.course_encs(course).await {
                Ok(encs) => encs,
                Err(_) => continue,
            };
            let works = match self.client.work_list(course, &encs).await {
                Ok(works) => works,
                Err(_) => continue, // 单课失败不破坏整体，也不刷屏
            };
            works_total += works.len();
            let mut note_items: Vec<(crate::modules::chaoxing::source::ChaoxingWork, Option<String>)> = Vec::new();

            for work in works {
                let key = format!("{}#{}", course.course_id, work.work_id);
                let open = work.is_open();
                let existing = self.store.document("chaoxing_work", &key)?;

                // 截止时间：快照缓存优先；未交且缺失才拉详情页（请求预算）
                let mut deadline = existing
                    .as_ref()
                    .and_then(|doc| doc.value.get("end_at"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty());
                if open && deadline.is_none() {
                    deadline = self
                        .client
                        .work_deadline(&work.detail_url)
                        .await
                        .unwrap_or(None);
                }

                match existing {
                    None => {
                        let mut snapshot = serde_json::json!({
                            "course": course.title,
                            "title": work.title,
                            "status": work.status,
                            "end_at": deadline,
                            "first_seen": now,
                        });
                        self.store
                            .put_document("chaoxing_work", &key, None, &snapshot)?;
                        if open {
                            new_opens += 1;
                            lines.push(format!(
                                "- 📝{}｜{}｜{}｜截止 {}",
                                course.title,
                                work.title,
                                work.status,
                                deadline.as_deref().unwrap_or("未知"),
                            ));
                            if self.config.work_todo_sync && !work.status.contains("已过期") {
                                let end_ts = deadline
                                    .as_deref()
                                    .and_then(|s| platform_local_ts(s, offset));
                                let due_soon_flag = end_ts
                                    .map(|ts| ts - now <= urgent_window && ts > now)
                                    .unwrap_or(false);
                                match self.todos.add(
                                    TodoInput {
                                        title: format!("学习通｜{}｜{}", course.title, work.title),
                                        priority: if due_soon_flag {
                                            TodoPriority::High
                                        } else {
                                            TodoPriority::Normal
                                        },
                                        labels: vec!["学习通".into()],
                                        due_at: end_ts,
                                        remind_at: end_ts,
                                        source: Some(TodoSource {
                                            channel: "chaoxing".into(),
                                            source_id: Some(work.work_id.clone()),
                                        }),
                                    },
                                    now,
                                ) {
                                    Ok(item) => {
                                        snapshot["todo_id"] = serde_json::json!(item.id);
                                        // 回写 todo_id（CAS 失败仅影响回勾精度）
                                        if let Ok(Some(doc)) =
                                            self.store.document("chaoxing_work", &key)
                                        {
                                            let mut value = doc.value.clone();
                                            value["todo_id"] = serde_json::json!(item.id);
                                            let _ = self.store.put_document(
                                                "chaoxing_work",
                                                &key,
                                                Some(doc.revision),
                                                &value,
                                            );
                                        }
                                        todo_created += 1;
                                    }
                                    Err(err) => lines.push(format!(
                                        "- ⚠️{}｜{}｜待办创建失败: {}",
                                        course.title, work.title, err
                                    )),
                                }
                            }
                        }
                    }
                    Some(doc) => {
                        let prev_status = doc
                            .value
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let was_open = prev_status.contains('未');
                        if was_open && !open {
                            // 未交 → 已交：更新快照 + 回勾待办（一次性）
                            submitted += 1;
                            let mut value = doc.value.clone();
                            value["status"] = serde_json::json!(work.status);
                            value["updated_at"] = serde_json::json!(now);
                            let updated = self.store.put_document(
                                "chaoxing_work",
                                &key,
                                Some(doc.revision),
                                &value,
                            )?;
                            if updated {
                                lines.push(format!(
                                    "- ✅{}｜{}｜已提交（{}）",
                                    course.title, work.title, work.status
                                ));
                                if let Some(todo_id) =
                                    doc.value.get("todo_id").and_then(serde_json::Value::as_str)
                                {
                                    let _ = self.todos.complete(todo_id, now);
                                }
                            }
                        } else if open {
                            // 临期/过期每日最多提醒一次
                            let end_ts = deadline
                                .as_deref()
                                .and_then(|s| platform_local_ts(s, offset));
                            let is_overdue = end_ts.map(|ts| ts < now).unwrap_or(false);
                            let is_due_soon = end_ts
                                .map(|ts| ts - now <= urgent_window && ts > now)
                                .unwrap_or(false);
                            if is_overdue || is_due_soon {
                                let category = if is_overdue { "overdue" } else { "due" };
                                let alert_key = format!("{category}:{today}:{key}");
                                if self
                                    .store
                                    .document("chaoxing_work_alert", &alert_key)?
                                    .is_none()
                                {
                                    self.store.put_document(
                                        "chaoxing_work_alert",
                                        &alert_key,
                                        None,
                                        &serde_json::json!({ "ts": now }),
                                    )?;
                                    if is_overdue {
                                        overdue_total += 1;
                                        lines.push(format!(
                                            "- ⚠️{}｜{}｜已过期未交（截止 {}）",
                                            course.title,
                                            work.title,
                                            deadline.as_deref().unwrap_or("未知"),
                                        ));
                                    } else {
                                        due_soon += 1;
                                        lines.push(format!(
                                            "- ⏰{}｜{}｜{}内截止（{}）",
                                            course.title,
                                            work.title,
                                            self.config.work_due_days,
                                            deadline.as_deref().unwrap_or("未知"),
                                        ));
                                    }
                                }
                            }
                            // 截止时间若首次拿到，回写快照
                            if doc.value.get("end_at").and_then(serde_json::Value::as_str)
                                != deadline.as_deref()
                                && let Some(d) = deadline.as_deref()
                            {
                                let mut value = doc.value.clone();
                                value["end_at"] = serde_json::json!(d);
                                let _ = self.store.put_document(
                                    "chaoxing_work",
                                    &key,
                                    Some(doc.revision),
                                    &value,
                                );
                            }
                        }
                    }
                }
                note_items.push((work, deadline));
            }
            course_notes.push((course.title.clone(), note_items));
        }

        let vault_files = self.sync_vault(&course_notes, now)?;

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = self.notifier.as_ref()
        {
            let mut message = format!("【学习通作业】\n{}", lines.join("\n"));
            if vault_files > 0 {
                message.push_str(&format!("\n（已同步 {vault_files} 个笔记到 Obsidian）"));
            }
            notifier
                .send(&self.config.notify_target, None, &message)
                .await?;
            notified = true;
        }

        Ok(ChaoxingWorkPollReport {
            login_ok: true,
            courses: courses.len(),
            works: works_total,
            new_opens,
            due_soon,
            overdue: overdue_total,
            submitted,
            todo_created,
            vault_files,
            notified,
        })
    }

    /// 作业清单落 Vault：每课 `学习通作业.md` + 作业文件夹 `作业说明.md`，
    /// 再聚合两个平台快照写学期级 `作业总表.md`。内容无变化不提交。
    fn sync_vault(&self, course_notes: &CourseNotes, now: i64) -> Result<usize> {
        let Some(vault) = &self.vault else {
            return Ok(0);
        };
        let Some(base) = self
            .config
            .vault_course_dir
            .as_deref()
            .map(|s| s.trim_end_matches('/'))
        else {
            return Ok(0);
        };
        let offset = self.todos.options().offset_seconds;
        let mut written = 0usize;
        for (course_title, items) in course_notes {
            if items.is_empty() {
                continue;
            }
            let dir = self
                .config
                .vault_courses
                .get(course_title.as_str())
                .cloned()
                .unwrap_or_else(|| course_title.clone());
            let course_base = format!("{base}/{dir}");
            vault.write_file(
                &format!("{course_base}/学习通作业.md"),
                &render_work_course_note(course_title, items, now, offset),
            )?;
            written += 1;
            for (work, deadline) in items {
                let folder = work_folder_name(work, deadline);
                vault.write_file(
                    &format!("{course_base}/{folder}/作业说明.md"),
                    &render_work_task_note(course_title, work, deadline, now, offset),
                )?;
                written += 1;
            }
        }
        // 跨平台聚合：学习通 + 智慧树快照 → 学期级作业总表
        vault.write_file(
            &format!("{base}/作业总表.md"),
            &self.render_homework_dashboard(now, offset)?,
        )?;
        written += 1;
        if vault.commit_all("[ru41] chaoxing 课程作业同步")? {
            vault.push()?;
            Ok(written)
        } else {
            Ok(0) // 内容无变化，不提交不播报
        }
    }

    /// 跨平台作业总表：从 records 快照聚合（新平台接入时在此加一行映射）。
    fn render_homework_dashboard(&self, now: i64, offset_seconds: i32) -> Result<String> {
        let mut rows: Vec<(Option<String>, String, String, String, String, bool)> = Vec::new();
        // (end_at, platform, course_dir, course, title, open)
        for doc in self.store.documents("chaoxing_work", 500)? {
            let course = str_field(&doc.value, "course").unwrap_or_default();
            let title = str_field(&doc.value, "title").unwrap_or_default();
            let status = str_field(&doc.value, "status").unwrap_or_default();
            let end_at = str_field(&doc.value, "end_at");
            let open = status.contains('未');
            rows.push((end_at, "学习通".into(), course.clone(), course, title, open));
        }
        for doc in self.store.documents("zhihuishu_task", 500)? {
            let course = str_field(&doc.value, "course").unwrap_or_default();
            let title = str_field(&doc.value, "title").unwrap_or_default();
            let end_at = str_field(&doc.value, "end_at");
            let completed = doc
                .value
                .get("completed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            rows.push((
                end_at,
                "智慧树".into(),
                course.clone(),
                course,
                title,
                !completed,
            ));
        }
        // 未完成在前按截止升序（空截止垫底），已完成按截止降序
        rows.sort_by(|a, b| b.5.cmp(&a.5).then(a.0.cmp(&b.0)));

        let stamp = local_stamp(now, offset_seconds);
        let mut out = String::new();
        out.push_str("---\n");
        out.push_str("tags:\n  - 作业\n  - 作业总表\n");
        out.push_str(&format!("updated: {stamp}\n---\n\n"));
        out.push_str("# 作业总表\n\n");
        out.push_str(
            "> 由 Ru41 跨平台聚合自动覆写（学习通 + 智慧树，新平台随接入追加）；请勿手改。\n\n",
        );
        let open: Vec<_> = rows.iter().filter(|r| r.5).collect();
        let done: Vec<_> = rows.iter().filter(|r| !r.5).collect();
        out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
        if open.is_empty() {
            out.push_str("当前没有未完成作业 🎉\n");
        }
        for (end_at, platform, dir, course, title, _) in &open {
            let overdue = end_at
                .as_deref()
                .and_then(|s| platform_local_ts(s, offset_seconds))
                .is_some_and(|ts| ts < now);
            let mark = if overdue { "⚠️已过期 " } else { "" };
            out.push_str(&format!(
                "- [ ] {mark}{platform}｜{course}｜{title} 📅 {}\n",
                end_at.as_deref().unwrap_or("未知")
            ));
            let _ = dir;
        }
        if !done.is_empty() {
            out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
            for (end_at, platform, _, course, title, _) in &done {
                out.push_str(&format!(
                    "- [x] {platform}｜{course}｜{title} 📅 {}\n",
                    end_at
                        .as_deref()
                        .map(|s| s.get(..10).unwrap_or(s))
                        .unwrap_or("未知")
                ));
            }
        }
        Ok(out)
    }
}

/// 平台时间串 "YYYY-MM-DD HH:MM"（北京时间）→ 本机固定偏移时间戳。
fn platform_local_ts(raw: &str, offset_seconds: i32) -> Option<i64> {
    use chrono::TimeZone;
    let naive = chrono::NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M").ok()?;
    chrono::FixedOffset::east_opt(offset_seconds)?
        .from_local_datetime(&naive)
        .single()
        .map(|dt| dt.timestamp())
}

/// 用户时区（固定偏移，容器安全）的 "%Y-%m-%d %H:%M" 时间戳。
fn local_stamp(now: i64, offset_seconds: i32) -> String {
    chrono::FixedOffset::east_opt(offset_seconds)
        .and_then(|tz| tz.timestamp_opt(now, 0).single())
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 作业文件夹名：`YYYY.MM.DD_作业名`（日期取截止日，缺省用作业名）。
fn work_folder_name(work: &crate::modules::chaoxing::source::ChaoxingWork, deadline: &Option<String>) -> String {
    let date = deadline
        .as_deref()
        .and_then(|s| s.get(..10))
        .map(|d| d.replace('-', "."));
    let title: String = work
        .title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '－'
            } else {
                c
            }
        })
        .collect();
    match date {
        Some(d) => format!("{d}_{title}"),
        None => title,
    }
}

/// 课程级索引：`学习通作业.md`（未完成/已完成清单，与智慧树作业.md 同构）。
fn render_work_course_note(
    course: &str,
    items: &[(crate::modules::chaoxing::source::ChaoxingWork, Option<String>)],
    now: i64,
    offset_seconds: i32,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str("platform: 学习通\n");
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str("tags:\n  - 课程\n  - 学习通\n---\n\n");
    out.push_str(&format!("# {course} · 学习通作业\n\n"));
    out.push_str("> 由 Ru41 每轮轮询自动覆写，请勿手改；截止时间为平台北京时间。\n\n");
    let open: Vec<_> = items.iter().filter(|(w, _)| w.is_open()).collect();
    let done: Vec<_> = items.iter().filter(|(w, _)| !w.is_open()).collect();
    out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
    if open.is_empty() {
        out.push_str("当前没有未完成作业 🎉\n");
    }
    for (work, deadline) in &open {
        let folder = work_folder_name(work, deadline);
        out.push_str(&format!(
            "- [ ] [[{folder}/作业说明|{}]] 📅 {}\n",
            work.title,
            deadline.as_deref().unwrap_or("未知")
        ));
    }
    if !done.is_empty() {
        out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
        for (work, deadline) in &done {
            let folder = work_folder_name(work, deadline);
            let date = deadline
                .as_deref()
                .and_then(|s| s.get(..10))
                .unwrap_or("未知");
            out.push_str(&format!(
                "- [x] [[{folder}/作业说明|{}]] 📅 {date}\n",
                work.title
            ));
        }
    }
    out
}

/// 作业说明页：元信息 frontmatter + 平台链接（说明文字 v1 不抓取正文）。
fn render_work_task_note(
    course: &str,
    work: &crate::modules::chaoxing::source::ChaoxingWork,
    deadline: &Option<String>,
    now: i64,
    offset_seconds: i32,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let status = if work.is_open() { "open" } else { "done" };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("task: {}\n", work.title));
    if let Some(end) = deadline {
        out.push_str(&format!("due: \"{end}\"\n"));
    }
    out.push_str(&format!("status: {status}\n"));
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str("tags:\n  - 课程\n  - 学习通\n  - 作业\n---\n\n");
    out.push_str(&format!("# {}\n\n", work.title));
    out.push_str(&format!("- 课程：{course}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("- 截止：{end}（北京时间）\n"));
    }
    out.push_str(&format!("- 状态：{}\n", work.status));
    out.push_str(&format!(
        "\n[平台作业页]({})\n\n> 由 Ru41 自学习通作业列表自动生成与更新。\n",
        work.detail_url
    ));
    out
}

impl crate::ingress::schedule::PollJob for ChaoxingWorkTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.work_poll_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}
