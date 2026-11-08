//! 作业监控跨平台共享件：学习通/微助教/智慧树三个作业 tracker 原本各自
//! 抄了一份的渲染与时间解析函数。平台差异用参数吸收（平台名、截止串、
//! 防重名 id、状态文案、页脚），不引入 trait 抽象。
//! （整体插件化后，多个作业插件共享此件，必须留在 lib 层。）

use crate::ports::{storage::TaskStore, vault::VaultTaskDirs};
use anyhow::Result;
use std::sync::Arc;

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
    _now: i64,
    _offset_seconds: i32,
    dirs: VaultTaskDirs,
) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("platform: {platform}\n"));
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
    body: Option<&str>,
    footer: &str,
    _now: i64,
    _offset_seconds: i32,
) -> String {
    let status = if open { "open" } else { "done" };
    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("course: {course}\n"));
    out.push_str(&format!("task: {title}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("due: \"{end}\"\n"));
    }
    out.push_str(&format!("status: {status}\n"));
    out.push_str(&format!(
        "tags:\n  - 课程\n  - {platform}\n  - 作业\n---\n\n"
    ));
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- 课程：{course}\n"));
    if let Some(end) = deadline {
        out.push_str(&format!("- 截止：{end}（北京时间）\n"));
    }
    out.push_str(&format!("- 状态：{status_label}\n"));
    // 作业正文：题干文字与配图（图片已落到作业目录的「附件」下，这里相对引用）。
    if let Some(body) = body {
        let body = body.trim();
        if !body.is_empty() {
            out.push_str("\n## 作业内容\n\n");
            out.push_str(body);
            out.push('\n');
        }
    }
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
            homework_status_open(status),
        ));
    }
    // 未完成在前按截止升序（空截止垫底），已完成按截止降序
    rows.sort_by(|a, b| b.5.cmp(&a.5).then(a.0.cmp(&b.0)));

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str("tags:\n  - 作业\n  - 作业总表\n");
    out.push_str("---\n\n");
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

/// 需要动作的作业状态（微助教口径）：1 未完成 / 2 被打回 / 7 补交中。
/// 其余（已超时/已提交/评分中/已公布/已批阅）视为收敛。
pub fn homework_status_open(status: i64) -> bool {
    matches!(status, 1 | 2 | 7)
}

// ---------------------------------------------------------------------------
// 临期作业的升级提醒：交互卡片（确认按钮）+ 未确认定时追发。
// 纯文本提醒发一次就完，用户没看到就是事故（2026-11-08 学习通英语作业
// 截止当天零提醒——虽然那次另有解析 bug）。首次进入临期窗口就发带按钮的
// 卡片（知道了 / 已完成），未确认每 REMINDER_NUDGE_INTERVAL 追发一次，
// 直到确认、作业提交（插件调 close_reminder）或过期兜底。
// 卡片复用核心 interaction 记录 + dispatcher 管线，插件不碰渠道细节。

use crate::domain::{NotifyRoute, records::Interaction};
use serde_json::{Value, json};

/// 追发间隔：未确认 2 小时再提醒。
pub const REMINDER_NUDGE_INTERVAL: i64 = 2 * 3600;
/// 截止后宽限：超过就停止追发与答复，交给过期的每日提醒路径。
pub const REMINDER_OVERDUE_GRACE: i64 = 12 * 3600;

/// 一次升级提醒推进的结果（插件写日志行/回勾待办用）。
#[derive(Debug, Clone, PartialEq)]
pub enum ReminderAction {
    /// 首次创建卡片。
    Created,
    /// 未确认到点追发。
    Nudged,
    /// 用户已确认（选项 JSON 原样带回，插件按 action 字段分支）。
    Acked(Value),
    /// 无事发生。
    Quiet,
}

fn reminder_alert_kind(plugin: &str) -> String {
    format!("{plugin}_reminder")
}

/// 交互 id：QQ 答复按前 8 位前缀定位，前几位必须唯一——
/// 插件代码 + 作业键的字母数字尾段（变化都在尾部的工作 id 上）。
fn reminder_interaction_id(plugin_code: &str, work_key: &str) -> String {
    let alnum: String = work_key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let skip = alnum.len().saturating_sub(10);
    format!("{plugin_code}{}", &alnum[skip..])
}

fn set_alert_state(
    store: &Arc<dyn TaskStore>,
    kind: &str,
    key: &str,
    state: &str,
    now: i64,
) -> Result<()> {
    let existing = store.document(kind, key)?;
    let mut value = existing
        .as_ref()
        .map(|d| d.value.clone())
        .unwrap_or_else(|| json!({}));
    value["state"] = json!(state);
    value["last_nudge"] = json!(now);
    // CAS 失败（并发轮）下轮再写，不算错误。
    let expected = existing.as_ref().map(|d| d.revision);
    let _ = store.put_document(kind, key, expected, &value)?;
    Ok(())
}

/// 进入临期窗口的作业每轮调一次：首次发卡片，之后按确认/追发/收口推进。
/// `plugin_code` 取两个独特字母（chaoxing→"cx"、zhihuishu→"zs"），
/// 保证交互 id 前 8 位全局唯一（QQ 答复按前缀定位）。
#[allow(clippy::too_many_arguments)]
pub fn remind_escalating(
    store: &Arc<dyn TaskStore>,
    plugin: &str,
    plugin_code: &str,
    work_key: &str,
    method: &str,
    detail: Value,
    deadline_ts: i64,
    route: &NotifyRoute,
    now: i64,
) -> Result<ReminderAction> {
    let alert_kind = reminder_alert_kind(plugin);
    let interaction_id = reminder_interaction_id(plugin_code, work_key);

    // 已收口（确认/关闭/过期）不再动作。
    if let Some(alert) = store.document(&alert_kind, work_key)?
        && alert.value["state"] != "pending"
    {
        return Ok(ReminderAction::Quiet);
    }

    let existing = store.document("interaction", &interaction_id)?;
    if let Some(doc) = &existing {
        let interaction: Interaction = serde_json::from_value(doc.value.clone())?;
        if let Some(response) = interaction.response {
            set_alert_state(store, &alert_kind, work_key, "acked", now)?;
            return Ok(ReminderAction::Acked(response));
        }
        if interaction.status != "pending" {
            set_alert_state(store, &alert_kind, work_key, "closed", now)?;
            return Ok(ReminderAction::Quiet);
        }
    }

    // 过宽限：收口，交还给过期的每日提醒路径。
    if now >= deadline_ts + REMINDER_OVERDUE_GRACE {
        set_alert_state(store, &alert_kind, work_key, "expired", now)?;
        return Ok(ReminderAction::Quiet);
    }

    let Some(doc) = existing else {
        let interaction = Interaction {
            id: interaction_id,
            run_id: format!("plugin:{plugin}"),
            request_id: json!(work_key),
            method: method.into(),
            detail,
            options: vec![
                json!({"label": "知道了", "action": "ack"}),
                json!({"label": "已完成", "action": "done"}),
            ],
            expires_at: deadline_ts + REMINDER_OVERDUE_GRACE,
            status: "pending".into(),
            response: None,
            notify: Some(route.clone()),
            dismiss_response: None,
            notified: false,
        };
        store.put_document(
            "interaction",
            &interaction.id,
            None,
            &serde_json::to_value(&interaction)?,
        )?;
        set_alert_state(store, &alert_kind, work_key, "pending", now)?;
        return Ok(ReminderAction::Created);
    };
    let last_nudge = store
        .document(&alert_kind, work_key)?
        .and_then(|a| a.value["last_nudge"].as_i64())
        .unwrap_or(0);
    if now - last_nudge < REMINDER_NUDGE_INTERVAL {
        return Ok(ReminderAction::Quiet);
    }
    // 追发：notified 置回 false，dispatcher 会重发卡片；正文顺带刷新。
    let mut value = doc.value.clone();
    value["notified"] = json!(false);
    value["detail"] = detail;
    if !store.put_document("interaction", &doc.id, Some(doc.revision), &value)? {
        return Ok(ReminderAction::Quiet); // 并发竞争，下轮再来
    }
    set_alert_state(store, &alert_kind, work_key, "pending", now)?;
    Ok(ReminderAction::Nudged)
}

/// 作业已提交/不再需要提醒：卡片与状态一起收口。
pub fn close_reminder(
    store: &Arc<dyn TaskStore>,
    plugin: &str,
    plugin_code: &str,
    work_key: &str,
    now: i64,
) -> Result<()> {
    let interaction_id = reminder_interaction_id(plugin_code, work_key);
    if let Some(doc) = store.document("interaction", &interaction_id)?
        && doc.value["status"] == "pending"
    {
        let mut value = doc.value.clone();
        value["status"] = json!("closed");
        let _ = store.put_document("interaction", &doc.id, Some(doc.revision), &value)?;
    }
    set_alert_state(store, &reminder_alert_kind(plugin), work_key, "closed", now)?;
    Ok(())
}

#[cfg(test)]
mod reminder_tests {
    use super::*;
    use crate::adapters::database::sqlite::SqliteTaskStore;

    fn store() -> (tempfile::TempDir, Arc<dyn TaskStore>) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
        (dir, Arc::new(store) as Arc<dyn TaskStore>)
    }

    fn route() -> NotifyRoute {
        NotifyRoute {
            channel: "qq".into(),
            target: "user:U1".into(),
            msg_id: None,
        }
    }

    /// 全生命周期：建卡 → 静默期不追发 → 到点追发 → 答复收口 → 终态静默。
    #[test]
    fn escalating_reminder_lifecycle() {
        let (_dir, store) = store();
        let now = 1_000_000;
        let deadline = now + 86_400;
        let detail = || json!({"course": "英语", "title": "作文", "deadline": "明天"});
        let created = remind_escalating(
            &store,
            "chaoxing",
            "cx",
            "1#w1",
            "作业临期提醒",
            detail(),
            deadline,
            &route(),
            now,
        )
        .unwrap();
        assert_eq!(created, ReminderAction::Created);
        let interaction = store
            .document("interaction", &reminder_interaction_id("cx", "1#w1"))
            .unwrap()
            .unwrap();
        assert_eq!(interaction.value["run_id"], "plugin:chaoxing");
        assert_eq!(interaction.value["notified"], false);

        // 静默期内：Quiet。
        let quiet = remind_escalating(
            &store,
            "chaoxing",
            "cx",
            "1#w1",
            "作业临期提醒",
            detail(),
            deadline,
            &route(),
            now + 600,
        )
        .unwrap();
        assert_eq!(quiet, ReminderAction::Quiet);

        // 到点：追发（notified 置回 false）。
        let nudged = remind_escalating(
            &store,
            "chaoxing",
            "cx",
            "1#w1",
            "作业临期提醒",
            detail(),
            deadline,
            &route(),
            now + REMINDER_NUDGE_INTERVAL + 1,
        )
        .unwrap();
        assert_eq!(nudged, ReminderAction::Nudged);
        let interaction = store
            .document("interaction", &reminder_interaction_id("cx", "1#w1"))
            .unwrap()
            .unwrap();
        assert_eq!(interaction.value["notified"], false);

        // 答复（模拟 dispatcher 已发卡、用户点按钮）→ Acked；之后静默。
        let id = reminder_interaction_id("cx", "1#w1");
        assert!(
            store
                .reply_interaction(&id, &json!({"label": "知道了", "action": "ack"}), now)
                .unwrap(),
            "plugin: 交互应可答复"
        );
        let acked = remind_escalating(
            &store,
            "chaoxing",
            "cx",
            "1#w1",
            "作业临期提醒",
            detail(),
            deadline,
            &route(),
            now + REMINDER_NUDGE_INTERVAL + 2,
        )
        .unwrap();
        assert!(matches!(acked, ReminderAction::Acked(_)));
        let after = remind_escalating(
            &store,
            "chaoxing",
            "cx",
            "1#w1",
            "作业临期提醒",
            detail(),
            deadline,
            &route(),
            now + REMINDER_NUDGE_INTERVAL + 3,
        )
        .unwrap();
        assert_eq!(after, ReminderAction::Quiet);
    }

    /// 提交后 close_reminder 收口，不再追发。
    #[test]
    fn close_reminder_stops_nudging() {
        let (_dir, store) = store();
        let now = 1_000_000;
        let deadline = now + 86_400;
        remind_escalating(
            &store,
            "zhihuishu",
            "zs",
            "c#t1",
            "作业临期提醒",
            json!({}),
            deadline,
            &route(),
            now,
        )
        .unwrap();
        close_reminder(&store, "zhihuishu", "zs", "c#t1", now + 1).unwrap();
        let action = remind_escalating(
            &store,
            "zhihuishu",
            "zs",
            "c#t1",
            "作业临期提醒",
            json!({}),
            deadline,
            &route(),
            now + REMINDER_NUDGE_INTERVAL + 1,
        )
        .unwrap();
        assert_eq!(action, ReminderAction::Quiet);
    }
}
