#[path = "common/consumer_contract.rs"]
mod contract;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::consumers::ConsumerSpec,
    ports::{consumers::ConsumerStore, storage::TaskStore},
};
use std::sync::{Arc, Barrier};

#[test]
fn sqlite_consumer_contract_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    contract::assert_consumer_contract(&store);
    let cursor = store.consumer("test.a").unwrap().unwrap().cursor;
    drop(store);
    let store = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(store.consumer("test.a").unwrap().unwrap().cursor, cursor);
    assert!(store.consumer("crash").unwrap().unwrap().blocked);
}

#[test]
fn independent_connections_cannot_claim_the_same_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    store.enqueue("event", "x").unwrap();
    store
        .register_consumer(&ConsumerSpec {
            name: "shared".into(),
            start_after: 0,
            max_attempts: 3,
        })
        .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = SqliteTaskStore::open(&path).unwrap();
                barrier.wait();
                store.claim_event("shared", 100, 30).unwrap()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .filter_map(|t| t.join().unwrap())
            .count(),
        1
    );
}

#[test]
fn upgrade_preserves_existing_event_log() {
    use ru41::ports::events::EventStore;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    for migration in [
        include_str!("../migrations/001_tasks.sql"),
        include_str!("../migrations/002_execution.sql"),
        include_str!("../migrations/003_backends.sql"),
        include_str!("../migrations/004_notify.sql"),
        include_str!("../migrations/005_records.sql"),
        include_str!("../migrations/006_events.sql"),
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
    let events = store.events_after(0, 10).unwrap();
    assert_eq!(events.len(), 1);
    store
        .register_consumer(&ConsumerSpec {
            name: "migrated".into(),
            start_after: 0,
            max_attempts: 3,
        })
        .unwrap();
    assert_eq!(
        store
            .claim_event("migrated", 100, 30)
            .unwrap()
            .unwrap()
            .event
            .id,
        events[0].id
    );
}
