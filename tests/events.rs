#[path = "common/event_contract.rs"]
mod contract;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    ports::{events::EventStore, storage::TaskStore},
};

#[test]
fn sqlite_event_lifecycle_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    contract::assert_event_contract(&store);
    let cursor = store.event_cursor().unwrap();
    drop(store);
    assert_eq!(
        SqliteTaskStore::open(&path)
            .unwrap()
            .event_cursor()
            .unwrap(),
        cursor
    );
}

#[test]
fn rollback_and_event_write_failure_leave_no_phantom_state() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO tasks(request_key,prompt) VALUES('rollback','private')",
        [],
    )
    .unwrap();
    assert_eq!(store.event_cursor().unwrap(), 0);
    tx.rollback().unwrap();
    assert!(store.events_after(0, 100).unwrap().is_empty());
    conn.execute_batch("CREATE TRIGGER fail_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    assert!(store.enqueue("fail", "private").is_err());
    assert!(store.list(10).unwrap().is_empty());
    conn.execute_batch("DROP TRIGGER fail_event;").unwrap();
    store.enqueue("ok", "private").unwrap();
    let other = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(other.events_after(0, 100).unwrap().len(), 1);
}

#[test]
fn upgrade_keeps_existing_tasks_without_inventing_historical_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    for migration in [
        include_str!("../migrations/001_tasks.sql"),
        include_str!("../migrations/002_execution.sql"),
        include_str!("../migrations/003_backends.sql"),
        include_str!("../migrations/004_notify.sql"),
        include_str!("../migrations/005_records.sql"),
    ] {
        conn.execute_batch(migration).unwrap();
    }
    conn.execute(
        "INSERT INTO tasks(request_key,prompt) VALUES('old','old')",
        [],
    )
    .unwrap();
    drop(conn);
    let store = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(store.list(10).unwrap().len(), 1);
    assert!(store.events_after(0, 10).unwrap().is_empty());
    store.cancel(1).unwrap();
    assert_eq!(store.events_after(0, 10).unwrap()[0].kind, "task.cancelled");
}
