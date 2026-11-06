//! 学习通作业监控：进课取 enc → 作业列表 → 未交分类 → 通知/待办/Obsidian。
//! 与智慧树作业模块同构，共享同一 Vault 学期目录并聚合作业总表。
//! 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。

use crate::config::ChaoxingConfig;
use crate::source::WorkContent;
use crate::source::{ChaoxingClient, ChaoxingWork};
use anyhow::Result;
use ru41::app::homework_work::{self as work_common, WorkNoteItem};
use ru41::app::todo::TodoService;
use ru41::ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault};
use serde::Serialize;
use std::sync::Arc;

/// 学习通截止时间格式（北京时间）：列表/详情页均为 "YYYY-MM-DD HH:MM"。
const DEADLINE_FORMATS: &[&str] = &["%Y-%m-%d %H:%M"];

/// vault 渲染缓存：(课程名, [(作业, 截止)]) 列表。
type CourseNotes = Vec<(
    String,
    Vec<(ChaoxingWork, Option<String>, Option<WorkContent>)>,
)>;

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
/// 每门课写 `学习通作业.md` 课程索引 + `{截止日}_{作业名}/作业说明.md`；
/// 渲染与时间解析共用 `homework::work_common`，学期级 `作业总表.md`
/// 由该共享层跨平台聚合（后续新作业平台照此接入）。
///
/// 红线：作业只做提醒/跟踪/归档，不做自动作答（自动答题=代写）。
/// 请求预算：每课 2 个只读请求（middle+list），详情页只对未交作业拉取；
/// 平台返回无权限/3004 时跳过该课并不刷屏（下轮重试）。
#[derive(Clone)]
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
        todo_options: ru41::domain::todo::TodoOptions,
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

    /// 课表短名：平台课名先过映射表（与课程目录同名），对外展示用课表名。
    fn display_course(&self, title: &str) -> String {
        self.config
            .vault_courses
            .get(title)
            .cloned()
            .unwrap_or_else(|| title.to_owned())
    }

    pub async fn poll_once(&self) -> Result<ChaoxingWorkPollReport> {
        use ru41::domain::todo::{TodoInput, TodoPriority, TodoSource};
        let now = ru41::util::now();
        let today = ru41::util::today_utc(now);
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
            let mut note_items: Vec<(
                crate::source::ChaoxingWork,
                Option<String>,
                Option<WorkContent>,
            )> = Vec::new();

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
                            "course": self.display_course(&course.title),
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
                                self.display_course(&course.title),
                                work.title,
                                work.status,
                                deadline.as_deref().unwrap_or("未知"),
                            ));
                            if self.config.work_todo_sync && !work.status.contains("已过期") {
                                let end_ts = deadline.as_deref().and_then(|s| {
                                    work_common::platform_ts(s, offset, DEADLINE_FORMATS)
                                });
                                let due_soon_flag = end_ts
                                    .map(|ts| ts - now <= urgent_window && ts > now)
                                    .unwrap_or(false);
                                match self.todos.add(
                                    TodoInput {
                                        title: format!(
                                            "学习通｜{}｜{}",
                                            self.display_course(&course.title),
                                            work.title
                                        ),
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
                    Some(mut doc) => {
                        let prev_status = doc
                            .value
                            .get("status")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let was_open = prev_status.contains('未');
                        // 存量快照里的平台课名刷成课表名（映射表改了才需要改一次），
                        // 否则作业总表里一直显示平台旧名。
                        let mapped = self.display_course(&course.title);
                        let stored = doc
                            .value
                            .get("course")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if stored != mapped {
                            let mut value = doc.value.clone();
                            value["course"] = serde_json::json!(mapped);
                            if self.store.put_document(
                                "chaoxing_work",
                                &key,
                                Some(doc.revision),
                                &value,
                            )? {
                                // 后续 CAS 用新 revision。
                                if let Ok(Some(fresh)) = self.store.document("chaoxing_work", &key)
                                {
                                    doc = fresh;
                                }
                            }
                        }
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
                            let end_ts = deadline.as_deref().and_then(|s| {
                                work_common::platform_ts(s, offset, DEADLINE_FORMATS)
                            });
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
                                            self.display_course(&course.title),
                                            work.title,
                                            deadline.as_deref().unwrap_or("未知"),
                                        ));
                                    } else {
                                        due_soon += 1;
                                        lines.push(format!(
                                            "- ⏰{}｜{}｜{}内截止（{}）",
                                            self.display_course(&course.title),
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
                // 题干与配图：截止时间与正文在同一个详情页，缓存进快照，
                // 只在首次（或平台改了正文）时重新拉。
                let mut content: Option<WorkContent> = self
                    .store
                    .document("chaoxing_work", &key)?
                    .and_then(|doc| doc.value.get("content").cloned())
                    .and_then(|v| serde_json::from_value::<WorkContent>(v).ok());
                if content.is_none() && open {
                    match self.client.work_content(&work.detail_url).await {
                        Ok(fresh) => {
                            if let Ok(Some(doc)) = self.store.document("chaoxing_work", &key) {
                                let mut value = doc.value.clone();
                                value["content"] = serde_json::to_value(&fresh)?;
                                let _ = self.store.put_document(
                                    "chaoxing_work",
                                    &key,
                                    Some(doc.revision),
                                    &value,
                                );
                            }
                            content = Some(fresh);
                        }
                        Err(error) => {
                            eprintln!("[chaoxing] {} 正文抓取失败: {error:#}", work.title);
                        }
                    }
                }
                note_items.push((work, deadline, content));
            }
            course_notes.push((course.title.clone(), note_items));
        }

        let vault_files = self.sync_vault(&course_notes, now).await?;

        let mut notified = false;
        if !lines.is_empty()
            && let Some(notifier) = self.notifier.as_ref()
        {
            // 同步成果只进报告与日志，不进推送（噪音）
            let message = format!(
                "## 📝 学习通作业（{}）\n\n{}",
                lines.len(),
                lines.join("\n")
            );
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

    /// 作业正文渲染：题干文字 + 图片。
    ///
    /// 图片先落到作业目录的「附件」子目录，笔记里用相对路径引用——
    /// Obsidian 直接能看，图片也不会因为平台链接过期而失效。
    async fn render_work_body(
        &self,
        course_base: &str,
        folder: &str,
        content: Option<&WorkContent>,
    ) -> Result<Option<String>> {
        let Some(content) = content else {
            return Ok(None);
        };
        let Some(vault) = &self.vault else {
            return Ok(None);
        };
        let mut parts: Vec<String> = Vec::new();
        let text = content.text.trim();
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
        if content.images.is_empty() {
            return Ok(Some(parts.join("\n\n")));
        }
        // 已经写过的图片不再重复下载：git 里有一份就直接引用。
        let head = vault.remote_head().ok();
        for (index, url) in content.images.iter().enumerate() {
            let name = image_file_name(url, index);
            let relative = format!("{course_base}/{folder}/附件/{name}");
            // 注意摊平：read_file_at 返回 Option<String>，文件不存在是 Ok(None)，
            // 直接 is_some() 会把「不存在」也当成「已存在」，图片就永远下不下来。
            let exists = head
                .as_ref()
                .and_then(|head| vault.read_file_at(head, &relative).ok())
                .flatten()
                .is_some();
            if !exists {
                match self.client.download_image(url).await {
                    Ok(bytes) => {
                        if let Err(error) = vault.write_bytes(&relative, &bytes) {
                            eprintln!("[chaoxing] 图片写入失败 {relative}: {error:#}");
                            continue;
                        }
                    }
                    Err(error) => {
                        eprintln!("[chaoxing] 图片下载失败 {url}: {error:#}");
                        continue;
                    }
                }
            }
            parts.push(format!("![{name}](附件/{name})"));
        }
        if parts.is_empty() {
            Ok(None)
        } else {
            Ok(Some(parts.join("\n\n")))
        }
    }

    /// 作业清单落 Vault：每课 `学习通作业.md` + 作业文件夹 `作业说明.md`，
    /// 再由 homework 共享层聚合三平台快照写学期级 `作业总表.md`。内容无变化不提交。
    async fn sync_vault(&self, course_notes: &CourseNotes, now: i64) -> Result<usize> {
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
            let dirs = self.config.work_vault_task_dirs;
            let rows: Vec<WorkNoteItem> = items
                .iter()
                .map(|(work, deadline, _)| WorkNoteItem {
                    title: work.title.clone(),
                    deadline: deadline.clone(),
                    open: work.is_open(),
                    id_fallback: None,
                })
                .collect();
            vault.write_file(
                &format!("{course_base}/学习通作业.md"),
                &work_common::render_work_course_note(
                    course_title,
                    "学习通",
                    &rows,
                    now,
                    offset,
                    dirs,
                ),
            )?;
            written += 1;
            for (work, deadline, content) in items {
                // substantive 策略：作业说明不含正文，只为未交作业建目录；
                // 已交作业的空壳目录纯属噪音，索引里降级为纯文本行。
                if dirs == ru41::ports::vault::VaultTaskDirs::Substantive && !work.is_open() {
                    continue;
                }
                let folder = work_common::work_folder_name(&work.title, deadline.as_deref(), None);
                let body = self
                    .render_work_body(&course_base, &folder, content.as_ref())
                    .await?;
                vault.write_file(
                    &format!("{course_base}/{folder}/作业说明.md"),
                    &work_common::render_work_task_note(
                        course_title,
                        "学习通",
                        &work.title,
                        &work.status,
                        work.is_open(),
                        deadline.as_deref(),
                        &work.detail_url,
                        body.as_deref(),
                        "由 Ru41 自学习通作业列表自动生成与更新。",
                        now,
                        offset,
                    ),
                )?;
                written += 1;
            }
        }
        // 跨平台聚合：学习通 + 智慧树 + 微助教快照 → 学期级作业总表
        vault.write_file(
            &format!("{base}/作业总表.md"),
            &work_common::render_homework_dashboard(self.store.as_ref(), now, offset)?,
        )?;
        written += 1;
        if vault.commit_all("[ru41] chaoxing 课程作业同步")? {
            vault.push()?;
            Ok(written)
        } else {
            Ok(0) // 内容无变化，不提交不播报
        }
    }
}

/// 图片落盘的文件名：取 URL 末段（去查询串），没有就用序号兜底。
fn image_file_name(url: &str, index: usize) -> String {
    let path = url.split('?').next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or_default();
    let name = last.trim();
    if name.is_empty() || !name.contains('.') {
        return format!("图{index}.jpg");
    }
    name.to_owned()
}

impl ru41::ingress::schedule::PollJob for ChaoxingWorkTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.work_poll_secs)
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
