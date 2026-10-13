use ru41::{adapters::sqlite::SqliteTaskStore, app::task_service::TaskService, domain::TaskStatus};
use std::sync::{Arc, Barrier};

#[test]
fn requests_survive_restart_and_retries_do_not_resurrect_cancelled_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let id = {
        let mut service = TaskService::new(Arc::new(SqliteTaskStore::open(&path).unwrap()));
        let first = service.enqueue("git:abc:note", "整理笔记").unwrap();
        assert!(first.created);
        let retry = service.enqueue("git:abc:note", "整理笔记").unwrap();
        assert!(!retry.created);
        assert_eq!(retry.task.id, first.task.id);
        service.cancel(first.task.id).unwrap();
        first.task.id
    };
    let mut service = TaskService::new(Arc::new(SqliteTaskStore::open(&path).unwrap()));
    assert_eq!(service.get(id).unwrap().status, TaskStatus::Cancelled);
    assert_eq!(service.cancel(id).unwrap().status, TaskStatus::Cancelled);
    let retry = service.enqueue("git:abc:note", "整理笔记").unwrap();
    assert!(!retry.created);
    assert_eq!(retry.task.status, TaskStatus::Cancelled);
    assert_eq!(service.list(20).unwrap().len(), 1);
}

#[test]
fn conflicting_keys_and_invalid_inputs_do_not_mutate_existing_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let mut service = TaskService::new(Arc::new(
        SqliteTaskStore::open(&dir.path().join("state.db")).unwrap(),
    ));
    let first = service.enqueue("event:1", "original").unwrap();
    assert!(service.enqueue("event:1", "different").is_err());
    assert!(service.enqueue(" ", "valid").is_err());
    assert!(service.enqueue("event:2", "  ").is_err());
    assert!(service.get(999).is_err());
    assert!(service.cancel(999).is_err());
    assert!(service.list(0).is_err());
    assert_eq!(service.get(first.task.id).unwrap().prompt, "original");
    assert_eq!(service.list(20).unwrap().len(), 1);
}

#[test]
fn concurrent_deliveries_create_exactly_one_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut service = TaskService::new(Arc::new(SqliteTaskStore::open(&path).unwrap()));
                service.enqueue("qq:event:1", "remember this").unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.created).count(), 1);
    assert!(results.iter().all(|r| r.task.id == results[0].task.id));
}

#[test]
fn future_database_version_is_rejected_without_downgrading() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 999).unwrap();
    assert!(SqliteTaskStore::open(&path).is_err());
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 999);
}
