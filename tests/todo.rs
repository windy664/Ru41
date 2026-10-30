use chrono::{FixedOffset, TimeZone};
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::todo::TodoService,
    domain::todo::{
        TodoInput, TodoOptions, TodoPriority, TodoSource, TodoStatus, TodoView, parse_due,
    },
    ports::storage::TaskStore,
};
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<dyn TaskStore>, TodoOptions, i64) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let options = TodoOptions {
        offset_seconds: 8 * 3600,
        recent_days: 7,
        long_term_days: 14,
    };
    let offset = FixedOffset::east_opt(options.offset_seconds).unwrap();
    let now = offset
        .with_ymd_and_hms(2026, 10, 19, 12, 0, 0)
        .unwrap()
        .timestamp();
    (dir, store, options, now)
}

fn add(
    service: &TodoService,
    title: &str,
    due_text: Option<&str>,
    now: i64,
    options: &TodoOptions,
) -> ru41::domain::todo::TodoItem {
    service
        .add(
            TodoInput {
                title: title.into(),
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at: due_text.map(|text| parse_due(text, now, options).unwrap()),
                remind_at: None,
                source: None,
            },
            now,
        )
        .unwrap()
}

fn add_homework(
    service: &TodoService,
    title: &str,
    due_text: Option<&str>,
    now: i64,
    options: &TodoOptions,
    channel: &str,
) -> ru41::domain::todo::TodoItem {
    service
        .add(
            TodoInput {
                title: title.into(),
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at: due_text.map(|text| parse_due(text, now, options).unwrap()),
                // 作业条目到期即提醒，验证到期提醒过滤有实际意义。
                remind_at: due_text.map(|text| parse_due(text, now, options).unwrap()),
                source: Some(TodoSource {
                    channel: channel.into(),
                    source_id: None,
                }),
            },
            now,
        )
        .unwrap()
}

#[test]
fn overview_computes_overdue_today_recent_long_term_and_inbox() {
    let (_dir, store, options, now) = fixture();
    let service = TodoService::new(store, options);
    let overdue = add(
        &service,
        "逾期事项",
        Some("2026-10-18 10:00"),
        now,
        &options,
    );
    let today = add(
        &service,
        "今日事项",
        Some("2026-10-19 22:00"),
        now,
        &options,
    );
    let recent = add(&service, "近期事项", Some("3天后"), now, &options);
    let long_term = add(&service, "长期事项", Some("30天后"), now, &options);
    let inbox = add(&service, "未排期事项", None, now, &options);
    let completed = add(
        &service,
        "已完成事项",
        Some("2026-10-19 10:00"),
        now,
        &options,
    );
    service.complete(completed.short_id(), now).unwrap();

    let overview = service.overview(now, 20).unwrap();
    assert_eq!(
        overview
            .overdue
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [overdue.id.as_str()]
    );
    assert_eq!(
        overview
            .today
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [today.id.as_str()]
    );
    assert_eq!(
        overview
            .recent
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [recent.id.as_str()]
    );
    assert_eq!(
        overview
            .long_term
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [long_term.id.as_str()]
    );
    assert_eq!(
        overview
            .inbox
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        [inbox.id.as_str()]
    );

    assert_eq!(service.list(TodoView::Overdue, now, 10).unwrap().len(), 1);
    assert_eq!(service.list(TodoView::Today, now, 10).unwrap().len(), 1);
    assert_eq!(service.list(TodoView::Recent, now, 10).unwrap().len(), 1);
    assert_eq!(service.list(TodoView::LongTerm, now, 10).unwrap().len(), 1);
    assert_eq!(service.list(TodoView::Inbox, now, 10).unwrap().len(), 1);
    assert_eq!(service.list(TodoView::All, now, 10).unwrap().len(), 6);
}

#[test]
fn complete_postpone_cancel_and_restore_are_persisted() {
    let (_dir, store, options, now) = fixture();
    let service = TodoService::new(store.clone(), options);
    let item = add(&service, "写周报", None, now, &options);

    let postponed = service
        .postpone(item.short_id(), "明天下午3点", now)
        .unwrap();
    assert!(postponed.due_at.unwrap() > now);
    assert_eq!(postponed.due_date(&options).as_deref(), Some("2026-10-20"));

    let reopened = TodoService::new(store.clone(), options);
    let listed = reopened.list(TodoView::Recent, now, 10).unwrap();
    assert!(listed.iter().any(|item| item.id == postponed.id));

    let completed = reopened.complete(postponed.short_id(), now).unwrap();
    assert_eq!(completed.status, TodoStatus::Completed);
    assert!(completed.completed_at.is_some());
    assert!(reopened.overview(now, 10).unwrap().active_count() == 0);

    let restored = reopened.restore(postponed.short_id(), now).unwrap();
    assert_eq!(restored.status, TodoStatus::Active);
    assert!(restored.completed_at.is_none());

    let cancelled = reopened.cancel(postponed.short_id(), now).unwrap();
    assert_eq!(cancelled.status, TodoStatus::Cancelled);
}

#[test]
fn selector_prefix_must_be_unique() {
    let (_dir, store, options, now) = fixture();
    let service = TodoService::new(store, options);
    let first = add(&service, "第一项", None, now, &options);
    let second = add(&service, "第二项", None, now, &options);

    assert!(service.complete(first.short_id(), now).is_ok());
    let error = service.complete(&first.short_id()[..2], now).unwrap_err();
    assert!(error.to_string().contains("ambiguous"));

    let other = service.complete(second.short_id(), now).unwrap();
    assert_eq!(other.title, "第二项");
}

#[test]
fn reminders_fire_once_and_postpone_rearms_them() {
    let (_dir, store, options, now) = fixture();
    let service = TodoService::new(store, options);
    let item = service
        .add(
            TodoInput {
                title: "已经到点的提醒".into(),
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at: Some(now - 60),
                remind_at: Some(now - 60),
                source: None,
            },
            now,
        )
        .unwrap();
    let completed = service
        .add(
            TodoInput {
                title: "已完成不提醒".into(),
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at: Some(now - 60),
                remind_at: Some(now - 60),
                source: None,
            },
            now,
        )
        .unwrap();
    service.complete(completed.short_id(), now).unwrap();

    let due = service.due_reminders(now, 10).unwrap();
    assert_eq!(
        due.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
        [item.id.as_str()]
    );
    service.mark_reminded(&item.id, now).unwrap();
    assert!(service.due_reminders(now, 10).unwrap().is_empty());

    let future = add(&service, "尚未到点", Some("明天下午3点"), now, &options);
    let postponed = service
        .postpone(future.short_id(), "后天下午3点", now)
        .unwrap();
    assert!(service.due_reminders(now, 10).unwrap().is_empty());
    assert_eq!(postponed.remind_at, postponed.due_at);
    assert!(postponed.last_reminded_at.is_none());
}

#[test]
fn due_parser_supports_iso_and_common_relative_dates() {
    let (_dir, _store, options, now) = fixture();
    let iso_date = parse_due("2026-10-20", now, &options).unwrap();
    assert_eq!(
        ru41::domain::todo::local_date(iso_date, options.offset_seconds),
        "2026-10-20"
    );
    let iso_datetime = parse_due("2026-10-20 15:30", now, &options).unwrap();
    assert_eq!(
        ru41::domain::todo::local_date(iso_datetime, options.offset_seconds),
        "2026-10-20"
    );
    let afternoon = parse_due("明天下午3点", now, &options).unwrap();
    assert_eq!(
        ru41::domain::todo::local_date(afternoon, options.offset_seconds),
        "2026-10-20"
    );
    let later = parse_due("3天后", now, &options).unwrap();
    assert_eq!(
        ru41::domain::todo::local_date(later, options.offset_seconds),
        "2026-10-22"
    );
    assert!(parse_due("不是时间", now, &options).is_err());
}

#[test]
fn homework_items_are_hidden_from_personal_todo_views() {
    let (_dir, store, options, now) = fixture();
    let service = TodoService::new(store, options);
    let due = parse_due("2026-10-18 10:00", now, &options).unwrap();
    let personal = service
        .add(
            TodoInput {
                title: "个人事项".into(),
                priority: TodoPriority::Normal,
                labels: Vec::new(),
                due_at: Some(due),
                remind_at: Some(due),
                source: None,
            },
            now,
        )
        .unwrap();
    let homework = add_homework(
        &service,
        "超星作业",
        Some("2026-10-18 10:00"),
        now,
        &options,
        "chaoxing",
    );
    let _zhihuishu = add_homework(&service, "智慧树作业", None, now, &options, "zhihuishu");
    let _teachermate = add_homework(&service, "学习通作业", None, now, &options, "teachermate");

    assert!(homework.is_homework());
    assert!(!personal.is_homework());

    for view in [
        TodoView::All,
        TodoView::Today,
        TodoView::Recent,
        TodoView::LongTerm,
        TodoView::Overdue,
        TodoView::Inbox,
    ] {
        let items = service.list(view, now, 100).unwrap();
        assert!(
            items.iter().all(|item| !item.is_homework()),
            "view {view:?} leaked homework items"
        );
        if view == TodoView::All {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].id, personal.id);
        }
    }

    let overview = service.overview(now, 100).unwrap();
    assert_eq!(overview.active_count(), 1);
    assert_eq!(overview.overdue[0].id, personal.id);

    let due = service.due_reminders(now, 100).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].id, personal.id);
}
