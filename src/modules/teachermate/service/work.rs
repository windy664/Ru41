//! 微助教作业监控：openid 直连 → 有课列表 → 单课作业清单 → 状态分类 →
//! 通知/待办/Obsidian。与学习通作业模块同构（docs/sources-chaoxing.md），
//! 快照字段名（course/title/status/end_at）刻意对齐，跨平台作业总表直接聚合约。
//! 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。

use crate::app::todo::TodoService;
use crate::modules::homework::work_common::{self, WorkNoteItem};
use crate::modules::teachermate::config::TeachermateConfig;
use crate::modules::teachermate::source::{
    CourseHomework, Homework, TeachermateClient, homework_status_open,
};
use crate::ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

/// vault 渲染缓存：(课程名, [(作业, 截止)]) 列表。
type CourseNotes = Vec<(String, Vec<(Homework, Option<String>)>)>;

#[derive(Debug, Serialize)]
pub struct TeachermateWorkPollReport {
    pub openid_ok: bool,
    pub courses: usize,
    pub works: usize,
    /// 本轮首次发现的未完成作业数。
    pub new_opens: usize,
    /// 本轮新进入临期窗口的作业数。
    pub due_soon: usize,
    /// 当前已过期未完成的作业数。
    pub overdue: usize,
    /// 本轮「未完成 → 已提交」变迁数（自动回勾待办）。
    pub submitted: usize,
    /// 其余状态变迁数（被打回/超时/补交开启等，各自只播一次）。
    pub status_changes: usize,
    pub todo_created: usize,
    pub vault_files: usize,
    pub notified: bool,
    /// openid_ok=false 时的失败原因（网络抖动重试后仍失败 / 鉴权失效）。
    pub error: Option<String>,
}

/// 微助教作业监控：复用签到模块的客户端（同一 openid），低频轮询：
/// 有作业的课程列表 → 单课作业清单 → 按状态分类（1/2/7 需动作）→ 通知/待办/Obsidian。
///
/// 状态语义（微信端 UI 状态表）：1 未完成、2 被打回、7 补交中需要动作；
/// 3 已超时、4/5 已提交、6 已公布成绩、8 已批阅视为收敛。
/// 临期/过期提醒每日最多一次（records kind `teachermate_work_alert`）。
pub struct TeachermateWorkTracker {
    store: Arc<dyn TaskStore>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    client: TeachermateClient,
    todos: TodoService,
    vault: Option<Arc<dyn Vault>>,
    config: TeachermateConfig,
}

const RECORD_KIND: &str = "teachermate_work";
const ALERT_KIND: &str = "teachermate_work_alert";

impl TeachermateWorkTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        client: TeachermateClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: TeachermateConfig,
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

    pub async fn poll_once(&self) -> Result<TeachermateWorkPollReport> {
        use crate::domain::todo::{TodoInput, TodoPriority, TodoSource};
        let now = crate::util::now();
        let today = crate::util::today_utc(now);
        let offset = self.todos.options().offset_seconds;

        let courses = match self.client.homework_courses().await {
            Ok(courses) => courses,
            Err(err) => {
                // 与签到模块共用 openid:{date} 告警键：一天最多告警一次
                let key = format!("openid:{today}");
                let nagged = self.store.document("teachermate_alert", &key)?.is_some();
                let mut notified = false;
                if !nagged
                    && err.to_string().contains("已失效")
                    && let Some(notifier) = self.notifier.as_ref()
                {
                    self.store.put_document(
                        "teachermate_alert",
                        &key,
                        None,
                        &serde_json::json!({ "ts": now }),
                    )?;
                    notifier
                        .send(
                            &self.config.notify_target,
                            None,
                            "⚠️ 微助教 openid 已失效（作业轮询）：`teachermate bind <链接>` 重新投喂",
                        )
                        .await?;
                    notified = true;
                }
                return Ok(TeachermateWorkPollReport {
                    openid_ok: false,
                    courses: 0,
                    works: 0,
                    new_opens: 0,
                    due_soon: 0,
                    overdue: 0,
                    submitted: 0,
                    status_changes: 0,
                    todo_created: 0,
                    vault_files: 0,
                    notified,
                    error: Some(err.to_string()),
                });
            }
        };

        let urgent_window = self.config.work_due_days * 86_400;
        let mut works_total = 0usize;
        let mut new_opens = 0usize;
        let mut due_soon = 0usize;
        let mut overdue_total = 0usize;
        let mut submitted = 0usize;
        let mut status_changes = 0usize;
        let mut todo_created = 0usize;
        let mut lines: Vec<String> = Vec::new();
        let mut course_notes: CourseNotes = Vec::new();

        for course in &courses {
            let works = match self.client.course_homeworks(course.course_id).await {
                Ok(CourseHomework { homeworks, .. }) => homeworks,
                Err(_) => continue, // 单课失败不破坏整体，也不刷屏
            };
            works_total += works.len();
            let mut note_items: Vec<(Homework, Option<String>)> = Vec::new();

            for work in works {
                let key = format!("{}#{}", course.course_id, work.h_id);
                let existing = self.store.document(RECORD_KIND, &key)?;
                match existing {
                    None => {
                        let snapshot = serde_json::json!({
                            "course": course.course,
                            "title": work.name,
                            "status": work.status,
                            "end_at": work.deadline,
                            "first_seen": now,
                        });
                        self.store
                            .put_document(RECORD_KIND, &key, None, &snapshot)?;
                        if work.is_open() {
                            new_opens += 1;
                            lines.push(format!(
                                "- 📝{}｜{}｜{}｜截止 {}",
                                course.course,
                                work.name,
                                work.status_label(),
                                work.deadline.as_deref().unwrap_or("未知"),
                            ));
                            if self.config.work_todo_sync {
                                let end_ts = work
                                    .deadline
                                    .as_deref()
                                    .and_then(|s| deadline_ts(s, offset));
                                let due_soon_flag = end_ts
                                    .map(|ts| ts - now <= urgent_window && ts > now)
                                    .unwrap_or(false);
                                match self.todos.add(
                                    TodoInput {
                                        title: format!("微助教｜{}｜{}", course.course, work.name),
                                        priority: if due_soon_flag {
                                            TodoPriority::High
                                        } else {
                                            TodoPriority::Normal
                                        },
                                        labels: vec!["微助教".into()],
                                        due_at: end_ts,
                                        remind_at: end_ts,
                                        source: Some(TodoSource {
                                            channel: "teachermate".into(),
                                            source_id: Some(work.h_id.to_string()),
                                        }),
                                    },
                                    now,
                                ) {
                                    Ok(item) => {
                                        // 回写 todo_id（CAS 失败仅影响回勾精度）
                                        if let Ok(Some(doc)) =
                                            self.store.document(RECORD_KIND, &key)
                                        {
                                            let mut value = doc.value.clone();
                                            value["todo_id"] = serde_json::json!(item.id);
                                            let _ = self.store.put_document(
                                                RECORD_KIND,
                                                &key,
                                                Some(doc.revision),
                                                &value,
                                            );
                                        }
                                        todo_created += 1;
                                    }
                                    Err(err) => lines.push(format!(
                                        "- ⚠️{}｜{}｜待办创建失败: {}",
                                        course.course, work.name, err
                                    )),
                                }
                            }
                        }
                    }
                    Some(doc) => {
                        let prev_status = doc
                            .value
                            .get("status")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(0);
                        if prev_status == work.status {
                            self.remind_due_window(
                                &key,
                                &work,
                                &course.course,
                                now,
                                &today,
                                offset,
                                urgent_window,
                                &mut due_soon,
                                &mut overdue_total,
                                &mut lines,
                            )?;
                            continue;
                        }

                        // 状态变迁：收敛（提交/超时）、恶化（被打回）、窗口（补交）
                        status_changes += 1;
                        let mut value = doc.value.clone();
                        value["status"] = serde_json::json!(work.status);
                        value["end_at"] = serde_json::json!(work.deadline);
                        value["updated_at"] = serde_json::json!(now);
                        let was_open = homework_status_open(prev_status);
                        let updated = self.store.put_document(
                            RECORD_KIND,
                            &key,
                            Some(doc.revision),
                            &value,
                        )?;
                        if !updated {
                            continue; // 并发写丢帧，下轮再收敛
                        }
                        let line = match (was_open, work.is_open(), work.status) {
                            // 未完成 → 已提交：回执 + 回勾待办
                            (true, false, s) if (4..=8).contains(&s) => {
                                submitted += 1;
                                if let Some(todo_id) =
                                    doc.value.get("todo_id").and_then(serde_json::Value::as_str)
                                {
                                    let _ = self.todos.complete(todo_id, now);
                                }
                                Some(format!(
                                    "- ✅{}｜{}｜已提交（{}）",
                                    course.course,
                                    work.name,
                                    work.submit_time.as_deref().unwrap_or("刚刚")
                                ))
                            }
                            // 未完成 → 已超时：漏交警示
                            (true, false, 3) => Some(format!(
                                "- ⚠️{}｜{}｜已超时未交（截止 {}）",
                                course.course,
                                work.name,
                                work.deadline.as_deref().unwrap_or("未知"),
                            )),
                            // 已提交 → 被打回：需要重交
                            (_, true, 2) => Some(format!(
                                "- ❗{}｜{}｜作业被打回，需重新提交（截止 {}）",
                                course.course,
                                work.name,
                                work.deadline.as_deref().unwrap_or("未知"),
                            )),
                            // 补交窗口开启（含 3 已超时 → 7）
                            (_, true, 7) => Some(format!(
                                "- 📝{}｜{}｜老师已允许补交（截止 {}）",
                                course.course,
                                work.name,
                                work.deadline.as_deref().unwrap_or("未知"),
                            )),
                            // 其余（评分中→已公布等）静默记录
                            _ => None,
                        };
                        if let Some(line) = line {
                            lines.push(line);
                        }
                    }
                }
                let dl = work.deadline.clone();
                note_items.push((work, dl));
            }
            course_notes.push((course.course.clone(), note_items));
        }

        let vault_files = self.sync_vault(&course_notes, now, offset)?;

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = self.notifier.as_ref()
        {
            let mut message = format!("【微助教作业】\n{}", lines.join("\n"));
            if vault_files > 0 {
                message.push_str(&format!("\n（已同步 {vault_files} 个笔记到 Obsidian）"));
            }
            notifier
                .send(&self.config.notify_target, None, &message)
                .await?;
            notified = true;
        }

        Ok(TeachermateWorkPollReport {
            openid_ok: true,
            courses: courses.len(),
            works: works_total,
            new_opens,
            due_soon,
            overdue: overdue_total,
            submitted,
            status_changes,
            todo_created,
            vault_files,
            notified,
            error: None,
        })
    }

    /// 未变化且仍需动作的作业：临期/过期每日最多提醒一次。
    #[allow(clippy::too_many_arguments)]
    fn remind_due_window(
        &self,
        key: &str,
        work: &Homework,
        course: &str,
        now: i64,
        today: &str,
        offset: i32,
        urgent_window: i64,
        due_soon: &mut usize,
        overdue_total: &mut usize,
        lines: &mut Vec<String>,
    ) -> Result<()> {
        if !work.is_open() {
            return Ok(());
        }
        let Some(end_ts) = work
            .deadline
            .as_deref()
            .and_then(|s| deadline_ts(s, offset))
        else {
            return Ok(());
        };
        let is_overdue = end_ts < now;
        let is_due_soon = end_ts - now <= urgent_window && end_ts > now;
        if !is_overdue && !is_due_soon {
            return Ok(());
        }
        let category = if is_overdue { "overdue" } else { "due" };
        let alert_key = format!("{category}:{today}:{key}");
        if self.store.document(ALERT_KIND, &alert_key)?.is_some() {
            return Ok(());
        }
        self.store.put_document(
            ALERT_KIND,
            &alert_key,
            None,
            &serde_json::json!({ "ts": now }),
        )?;
        if is_overdue {
            *overdue_total += 1;
            lines.push(format!(
                "- ⚠️{}｜{}｜已过期未交（截止 {}）",
                course,
                work.name,
                work.deadline.as_deref().unwrap_or("未知"),
            ));
        } else {
            *due_soon += 1;
            lines.push(format!(
                "- ⏰{}｜{}｜{}内截止（{}）",
                course,
                work.name,
                self.config.work_due_days,
                work.deadline.as_deref().unwrap_or("未知"),
            ));
        }
        Ok(())
    }

    /// 作业清单落 Vault：每课 `微助教作业.md` + 作业文件夹 `作业说明.md`。
    /// 跨平台作业总表由 homework 共享层聚合（teachermate_work 快照字段已对齐）。
    /// 内容无变化不提交。
    fn sync_vault(&self, course_notes: &CourseNotes, now: i64, offset: i32) -> Result<usize> {
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
            let dirs = self.config.work_vault_task_dirs;
            let rows: Vec<WorkNoteItem> = items
                .iter()
                .map(|(work, deadline)| WorkNoteItem {
                    title: work.name.clone(),
                    deadline: deadline.clone(),
                    open: work.is_open(),
                    id_fallback: Some(work.h_id),
                })
                .collect();
            vault.write_file(
                &format!("{course_base}/微助教作业.md"),
                &work_common::render_work_course_note(
                    course_title,
                    "微助教",
                    &rows,
                    now,
                    offset,
                    dirs,
                ),
            )?;
            written += 1;
            for (work, deadline) in items {
                // substantive 策略：作业说明不含正文，只为未交作业建目录；
                // 已交作业的空壳目录纯属噪音，索引里降级为纯文本行。
                if dirs == crate::ports::vault::VaultTaskDirs::Substantive && !work.is_open() {
                    continue;
                }
                let folder = work_common::work_folder_name(
                    work.name.as_str(),
                    deadline.as_deref(),
                    Some(work.h_id),
                );
                vault.write_file(
                    &format!("{course_base}/{folder}/作业说明.md"),
                    &work_common::render_work_task_note(
                        course_title,
                        "微助教",
                        &work.name,
                        work.status_label(),
                        work.is_open(),
                        deadline.as_deref(),
                        "https://www.teachermate.com.cn/pc-ssr/student/homework",
                        "由 Ru41 自微助教作业列表自动生成与更新；正文与提交请在微信服务号完成。",
                        now,
                        offset,
                    ),
                )?;
                written += 1;
            }
        }
        if vault.commit_all("[ru41] teachermate 课程作业同步")? {
            vault.push()?;
            Ok(written)
        } else {
            Ok(0) // 内容无变化，不提交不播报
        }
    }
}

/// 微助教截止时间原文 → 本机固定偏移时间戳。格式未实测，宽容尝试常见形态。
fn deadline_ts(raw: &str, offset_seconds: i32) -> Option<i64> {
    work_common::platform_ts(
        raw,
        offset_seconds,
        &[
            "%Y-%m-%d %H:%M:%S",
            "%Y-%m-%d %H:%M",
            "%Y/%m/%d %H:%M",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%dT%H:%M",
        ],
    )
    .or_else(|| {
        // Unix 秒时间戳兜底（毫秒先除）
        let ts = raw.trim().parse::<i64>().ok()?;
        Some(if ts > 1_000_000_000_000 {
            ts / 1000
        } else {
            ts
        })
        .filter(|ts| *ts > 1_000_000_000)
    })
}

impl crate::ingress::schedule::PollJob for TeachermateWorkTracker {
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
