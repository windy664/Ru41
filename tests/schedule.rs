//! 行程（日程事件）核心测试：周期展开、自然语言解析、服务 CRUD、提醒去重与冲突检测。

use ru41::adapters::database::sqlite::SqliteTaskStore;
use ru41::app::schedule::ScheduleService;
use ru41::domain::schedule::{
    EventInput, EventRecurrence, EventSource, EventStatus, event_time_label, parse_event_when,
};
use ru41::domain::todo::TodoOptions;
use std::sync::Arc;
use tempfile::TempDir;

const OFFSET: i32 = 8 * 3600;

/// 2026-10-20 12:00:00 +0800 的 Unix 秒（测试锚点）。
fn now_anchor() -> i64 {
    1_800_000_000 - (1_800_000_000 + OFFSET as i64).rem_euclid(86400) + 12 * 3600
}

fn options() -> TodoOptions {
    TodoOptions {
        offset_seconds: OFFSET,
        ..TodoOptions::default()
    }
}

fn fixture() -> (TempDir, Arc<SqliteTaskStore>) {
    let dir = TempDir::new().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
    (dir, Arc::new(store))
}

fn add_event(
    service: &ScheduleService,
    title: &str,
    starts_at: i64,
    ends_at: i64,
    recurrence: EventRecurrence,
    now: i64,
) -> ru41::domain::schedule::EventItem {
    service
        .add(
            EventInput {
                title: title.to_owned(),
                starts_at,
                ends_at,
                location: None,
                recurrence,
                source: Some(EventSource {
                    channel: "test".into(),
                    source_id: None,
                }),
            },
            now,
        )
        .unwrap()
}

#[test]
fn oneoff_occurrence_only_inside_window() {
    let now = now_anchor();
    let item = add_event_ctx("例会", now + 3600, now + 7200, EventRecurrence::None, now);
    assert_eq!(item.occurrences_between(now, now + 86400, OFFSET).len(), 1);
    assert!(
        item.occurrences_between(now + 7200, now + 86400, OFFSET)
            .is_empty()
    );
}

#[test]
fn weekly_expands_matching_weekdays_only() {
    let now = now_anchor();
    // 2026-10-20 是周二。每周二 + 周四，14:00-15:40。
    let base = now - 12 * 3600 + 14 * 3600; // 今天 14:00 本地
    let item = add_event_ctx(
        "体育课",
        base,
        base + 100 * 60,
        EventRecurrence::Weekly {
            weekday_mask: 0b0000_0101,
        }, // 周二+周四
        now,
    );
    let window = item.occurrences_between(now - 86400, now + 7 * 86400, OFFSET);
    // 一周内周二、周四各一次。
    assert_eq!(window.len(), 2);
    // 两次发生间隔 2 天。
    assert_eq!(window[1].starts_at - window[0].starts_at, 2 * 86400);
    // 每次时长 100 分钟。
    assert_eq!(window[0].ends_at - window[0].starts_at, 100 * 60);
    // 标记为周期发生。
    assert!(window[0].recurring);
}

#[test]
fn weekly_never_generates_before_base_day() {
    let now = now_anchor();
    // 每周一 8:00，但基准日设为下下周一：窗口早期不应出现发生。
    let base = now + 6 * 86400 + 8 * 3600 - 12 * 3600; // 下周一 8:00 本地
    let item = add_event_ctx(
        "升旗",
        base,
        base + 1800,
        EventRecurrence::Weekly {
            weekday_mask: 0b0000_0001,
        },
        now,
    );
    let window = item.occurrences_between(now, base, OFFSET);
    assert!(window.is_empty(), "base day 之前不得生成发生");
}

#[test]
fn parse_tomorrow_range() {
    let now = now_anchor();
    let (starts, ends, recurrence) = parse_event_when("明天14:00-16:00", now, &options()).unwrap();
    assert_eq!(recurrence, EventRecurrence::None);
    assert_eq!(ends - starts, 2 * 3600);
    // 开始于明天本地 14:00（锚点是明天 12:00）。
    let tomorrow = now + 86400;
    assert_eq!(starts - tomorrow, 2 * 3600);
}

#[test]
fn parse_weekly_multi_day() {
    let now = now_anchor();
    let (starts, ends, recurrence) =
        parse_event_when("每周三五六19:00-21:00", now, &options()).unwrap();
    assert_eq!(
        recurrence,
        EventRecurrence::Weekly {
            weekday_mask: 0b0011_0100
        } // 周三+周五+周六
    );
    assert_eq!(ends - starts, 2 * 3600);
    // 基准日必须是今天或未来。
    assert!(starts >= now - 86400);
}

#[test]
fn parse_crossing_midnight_end() {
    let now = now_anchor();
    let (starts, ends, _) = parse_event_when("今天22:00-1:00", now, &options()).unwrap();
    assert_eq!(ends - starts, 3 * 3600);
}

#[test]
fn parse_rejects_garbage() {
    let now = now_anchor();
    assert!(parse_event_when("随便聊聊", now, &options()).is_none());
    assert!(parse_event_when("", now, &options()).is_none());
}

#[test]
fn service_crud_and_cancel() {
    let (_dir, store) = fixture();
    let service = ScheduleService::new(store.clone(), options());
    let now = now_anchor();
    let item = add_event(
        &service,
        "班会",
        now + 3600,
        now + 7200,
        EventRecurrence::None,
        now,
    );
    assert_eq!(service.upcoming(now, 1, 10).unwrap().len(), 1);

    let cancelled = service.cancel(item.short_id(), now).unwrap();
    assert_eq!(cancelled.status, EventStatus::Cancelled);
    assert!(service.upcoming(now, 1, 10).unwrap().is_empty());
    // 重复取消报错。
    assert!(service.cancel(item.short_id(), now).is_err());
}

#[test]
fn service_conflict_detection() {
    let (_dir, store) = fixture();
    let service = ScheduleService::new(store.clone(), options());
    let now = now_anchor();
    add_event(
        &service,
        "例会",
        now + 3600,
        now + 7200,
        EventRecurrence::None,
        now,
    );
    // 重叠窗口命中。
    let hits = service.conflicts(now + 5400, now + 9000, now).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "例会");
    // 不重叠窗口无冲突。
    assert!(
        service
            .conflicts(now + 7200, now + 9000, now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn event_reminder_fires_once_per_occurrence() {
    let (_dir, store) = fixture();
    let service = ScheduleService::new(store.clone(), options());
    let now = now_anchor();
    add_event(
        &service,
        "约会",
        now + 600,
        now + 3600,
        EventRecurrence::None,
        now,
    );
    let due = service.due_event_reminders(now, 15 * 60, 10).unwrap();
    assert_eq!(due.len(), 1);
    // 标记后不再提醒。
    service.mark_reminded(&due[0], now).unwrap();
    assert!(
        service
            .due_event_reminders(now, 15 * 60, 10)
            .unwrap()
            .is_empty()
    );
    // 窗口外不提醒。
    let far = service.due_event_reminders(now - 3600, 60, 10).unwrap();
    assert!(far.is_empty());
}

#[test]
fn event_time_label_weekly_prefix() {
    let now = now_anchor();
    let label = event_time_label(now, true, OFFSET);
    assert!(label.starts_with("每"), "周期事件标签带每X前缀: {label}");
    let oneoff = event_time_label(now, false, OFFSET);
    assert!(oneoff.contains('-'), "一次性事件标签带日期: {oneoff}");
}

fn add_event_ctx(
    title: &str,
    starts_at: i64,
    ends_at: i64,
    recurrence: EventRecurrence,
    now: i64,
) -> ru41::domain::schedule::EventItem {
    let item = ru41::domain::schedule::EventItem {
        id: "evt_test_1".into(),
        title: title.to_owned(),
        status: EventStatus::Active,
        starts_at,
        ends_at,
        location: None,
        recurrence,
        reminded_starts: Vec::new(),
        created_at: now,
        updated_at: now,
        source: None,
    };
    item.validate().unwrap();
    item
}
