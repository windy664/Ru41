//! 作业监控跨平台共享件：学习通/微助教/智慧树三个作业 tracker 原本各自
//! 抄了一份的渲染与时间解析函数。平台差异用参数吸收（平台名、截止串、
//! 防重名 id、状态文案、页脚），不引入 trait 抽象。

use crate::ports::{storage::TaskStore, vault::VaultTaskDirs};
use anyhow::Result;

/// 用户时区（固定偏移，容器安全）的 "%Y-%m-%d %H:%M" 时间戳。
pub fn local_stamp(now: i64, offset_seconds: i32) -> String {
    use chrono::TimeZone;
    chrono::FixedOffset::east_opt(offset_seconds)
        .and_then(|tz| tz.timestamp_opt(now, 0).single())
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 平台时间串 → 本机固定偏移时间戳：按给定格式表逐个宽容尝试，
/// 第一个解析成功的格式生效。各平台的格式表留在调用侧。
pub fn platform_ts(raw: &str, offset_seconds: i32, formats: &[&str]) -> Option<i64> {
    use chrono::TimeZone;
    let raw = raw.trim();
    for fmt in formats {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(raw, fmt) {
            return chrono::FixedOffset::east_opt(offset_seconds)?
                .from_local_datetime(&naive)
                .single()
                .map(|dt| dt.timestamp());
        }
    }
    None
}

/// 文件系统不友好字符替换为全角「－」（作业名/附件名共用）。
pub fn sanitize_title(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '－'
            } else {
                c
            }
        })
        .collect()
}

/// 作业文件夹名：`YYYY.MM.DD_作业名`（日期取 date_source 前 10 位）。
/// 缺日期时用 id_fallback 前缀防重名（微助教传 h_id），再缺省只留作业名。
pub fn work_folder_name(
    title: &str,
    date_source: Option<&str>,
    id_fallback: Option<i64>,
) -> String {
    let date = date_source
        .and_then(|s| s.get(..10))
        .map(|d| d.replace('-', "."));
    let title = sanitize_title(title);
    match (date, id_fallback) {
        (Some(d), _) => format!("{d}_{title}"),
        (None, Some(id)) => format!("{id}_{title}"),
        (None, None) => title,
    }
}

/// 作业索引行：建目录的作业双链到作业文件夹；substantive 策略下已交作业
/// 没有目录，降级为纯文本行，避免 Obsidian 里挂着空链接。
pub fn work_line_item(
    title: &str,
    deadline: Option<&str>,
    dirs: VaultTaskDirs,
    done: bool,
    id_fallback: Option<i64>,
) -> String {
    let checked = if done { "x" } else { " " };
    let date = deadline.unwrap_or("未知");
    if dirs == VaultTaskDirs::All || !done {
        let folder = work_folder_name(title, deadline, id_fallback);
        format!("- [{checked}] [[{folder}/作业说明|{title}]] 📅 {date}\n")
    } else {
        format!("- [{checked}] {title} 📅 {date}\n")
    }
}

/// 课程索引渲染所需的作业行（平台无关快照）。
pub struct WorkNoteItem {
    pub title: String,
    pub deadline: Option<String>,
    pub open: bool,
    /// 无截止日时文件夹名的防重名前缀（微助教用 h_id）。
    pub id_fallback: Option<i64>,
}

/// 课程级索引：`{平台}作业.md`（未完成/已完成清单，各平台同构）。
pub fn render_work_course_note(
    course: &str,
    platform: &str,
    items: &[WorkNoteItem],
    now: i64,
    offset_seconds: i32,
    dirs: VaultTaskDirs,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("platform: {platform}\n"));
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str(&format!("tags:\n  - 课程\n  - {platform}\n---\n\n"));
    out.push_str(&format!("# {course} · {platform}作业\n\n"));
    out.push_str("> 由 Ru41 每轮轮询自动覆写，请勿手改；截止时间为平台北京时间。\n\n");
    let open: Vec<_> = items.iter().filter(|it| it.open).collect();
    let done: Vec<_> = items.iter().filter(|it| !it.open).collect();
    out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
    if open.is_empty() {
        out.push_str("当前没有未完成作业 🎉\n");
    }
    for item in &open {
        out.push_str(&work_line_item(
            &item.title,
            item.deadline.as_deref(),
            dirs,
            false,
            item.id_fallback,
        ));
    }
    if !done.is_empty() {
        out.push_str(&format!("\n## 已完成（{}）\n\n", done.len()));
        for item in &done {
            out.push_str(&work_line_item(
                &item.title,
                item.deadline.as_deref(),
                dirs,
                true,
                item.id_fallback,
            ));
        }
    }
    out
}

/// 作业说明页：元信息 frontmatter + 平台链接（正文 v1 不抓取）。
/// 状态文案与页脚由各平台给定（微助教附微信服务号指引）。
#[allow(clippy::too_many_arguments)]
pub fn render_work_task_note(
    course: &str,
    platform: &str,
    title: &str,
    status_label: &str,
    open: bool,
    deadline: Option<&str>,
    url: &str,
    footer: &str,
    now: i64,
    offset_seconds: i32,
) -> String {
    let stamp = local_stamp(now, offset_seconds);
    let status = if open { "open" } else { "done" };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("task: {title}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("due: \"{end}\"\n"));
    }
    out.push_str(&format!("status: {status}\n"));
    out.push_str(&format!("updated: {stamp}\n"));
    out.push_str(&format!(
        "tags:\n  - 课程\n  - {platform}\n  - 作业\n---\n\n"
    ));
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- 课程：{course}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("- 截止：{end}（北京时间）\n"));
    }
    out.push_str(&format!("- 状态：{status_label}\n"));
    out.push_str(&format!("\n[平台作业页]({url})\n\n> {footer}\n"));
    out
}

pub fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// 跨平台作业总表：从 records 快照聚合（学习通 + 智慧树 + 微助教），
/// 由 chaoxing 作业轮询在写课程索引后顺带覆写学期级 `作业总表.md`。
pub fn render_homework_dashboard(
    store: &dyn TaskStore,
    now: i64,
    offset_seconds: i32,
) -> Result<String> {
    let mut rows: Vec<(Option<String>, String, String, String, String, bool)> = Vec::new();
    // (end_at, platform, course_dir, course, title, open)
    for doc in store.documents("chaoxing_work", 500)? {
        let course = str_field(&doc.value, "course").unwrap_or_default();
        let title = str_field(&doc.value, "title").unwrap_or_default();
        let status = str_field(&doc.value, "status").unwrap_or_default();
        let end_at = str_field(&doc.value, "end_at");
        let open = status.contains('未');
        rows.push((end_at, "学习通".into(), course.clone(), course, title, open));
    }
    for doc in store.documents("zhihuishu_task", 500)? {
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
    // 微助教：快照字段与学习通对齐（course/title/end_at），status 为状态数字
    for doc in store.documents("teachermate_work", 500)? {
        let course = str_field(&doc.value, "course").unwrap_or_default();
        let title = str_field(&doc.value, "title").unwrap_or_default();
        let end_at = str_field(&doc.value, "end_at");
        let status = doc
            .value
            .get("status")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        rows.push((
            end_at,
            "微助教".into(),
            course.clone(),
            course,
            title,
            super::homework_status_open(status),
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
    out.push_str("> 由 Ru41 跨平台聚合自动覆写（学习通 + 智慧树 + 微助教）；请勿手改。\n\n");
    let open: Vec<_> = rows.iter().filter(|r| r.5).collect();
    let done: Vec<_> = rows.iter().filter(|r| !r.5).collect();
    out.push_str(&format!("## 未完成（{}）\n\n", open.len()));
    if open.is_empty() {
        out.push_str("当前没有未完成作业 🎉\n");
    }
    for (end_at, platform, dir, course, title, _) in &open {
        let overdue = end_at
            .as_deref()
            .and_then(|s| platform_ts(s, offset_seconds, &["%Y-%m-%d %H:%M"]))
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
