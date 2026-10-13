use ru41::{
    adapters::sqlite::SqliteTaskStore,
    app::store::TaskStore,
    domain::{AgentResult, TaskStatus},
};
use std::sync::{Arc, Barrier};

#[test]
fn upgrade_preserves_v1_ids_and_cancelled_requests() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("../migrations/001_tasks.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO tasks(id,request_key,prompt,status) VALUES(42,'old','hello','cancelled')",
        [],
    )
    .unwrap();
    drop(conn);
    let store = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(
        store.get(42).unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    let added = store.enqueue("new", "next").unwrap();
    assert!(added.task.id > 42);
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    assert_eq!(claim.task.id, added.task.id);
}

#[test]
fn multiple_workers_cannot_claim_the_same_task() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    SqliteTaskStore::open(&path)
        .unwrap()
        .enqueue("event", "hello")
        .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = SqliteTaskStore::open(&path).unwrap();
                barrier.wait();
                store
                    .claim(&format!("worker-{i}"), 100, 30)
                    .unwrap()
                    .map(|c| c.attempt_id)
            })
        })
        .collect();
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .count(),
        1
    );
}

#[test]
fn expired_owner_is_fenced_and_recovery_keeps_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let a = SqliteTaskStore::open(&path).unwrap();
    a.enqueue("event", "hello").unwrap();
    let old = a.claim("old", 100, 10).unwrap().unwrap();
    assert!(!a.heartbeat(&old, 110, 10).unwrap());
    let b = SqliteTaskStore::open(&path).unwrap();
    let new = b.claim("new", 111, 30).unwrap().unwrap();
    let result = AgentResult {
        summary: "done".into(),
    };
    assert!(!a.succeed(&old, &result, None, 112).unwrap());
    assert!(!a.fail(&old, "late failure", 112, 10, false).unwrap());
    assert!(b.succeed(&new, &result, Some("thread"), 112).unwrap());
    let attempts = b.attempts(new.task.id).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].status, "interrupted");
    assert_eq!(attempts[1].session_id.as_deref(), Some("thread"));
    assert_eq!(
        b.get(new.task.id).unwrap().unwrap().result.unwrap().summary,
        "done"
    );
}

#[test]
fn cancellation_fences_completion_and_success_cannot_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
    let task = store.enqueue("event", "hello").unwrap().task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store.cancel(task.id).unwrap();
    assert!(
        !store
            .succeed(
                &claim,
                &AgentResult {
                    summary: "late".into()
                },
                None,
                101
            )
            .unwrap()
    );
    assert_eq!(store.attempts(task.id).unwrap()[0].status, "cancelled");
    assert!(store.retry(task.id).is_err());
    let next = store.enqueue("event2", "hello").unwrap().task;
    let claim = store.claim("worker", 101, 30).unwrap().unwrap();
    assert!(
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: "done".into()
                },
                None,
                102
            )
            .unwrap()
    );
    assert!(store.cancel(next.id).is_err());
}

#[test]
fn retry_delay_budget_and_manual_retry_preserve_attempts() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
    let id = store.enqueue("event", "hello").unwrap().task.id;
    for i in 0..3 {
        let now = 100 + i * 20;
        let claim = store.claim("worker", now, 30).unwrap().unwrap();
        store.fail(&claim, "offline", now + 1, 10, false).unwrap();
        assert!(store.claim("worker", now + 2, 30).unwrap().is_none());
    }
    assert_eq!(store.get(id).unwrap().unwrap().status, TaskStatus::Failed);
    assert!(store.claim("worker", 500, 30).unwrap().is_none());
    store.retry(id).unwrap();
    let claim = store.claim("worker", 501, 30).unwrap().unwrap();
    assert_eq!(claim.task.attempt_count, 4);
    assert_eq!(store.attempts(id).unwrap().len(), 4);
}

#[test]
fn notify_route_is_enqueued_once_and_marked_after_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteTaskStore::open(&dir.path().join("state.db")).unwrap();
    let route = ru41::domain::NotifyRoute {
        channel: "qq".into(),
        target: "group:G1".into(),
        msg_id: Some("msg-1".into()),
    };
    let added = store
        .enqueue_with_notify(
            "qq:msg:msg-1",
            "hello",
            "codex",
            ru41::domain::RunContract::AnalysisV1,
            &route,
        )
        .unwrap();
    assert!(added.created);
    assert_eq!(added.task.notify.as_ref(), Some(&route));
    // 重复投递幂等，路由不同的同键请求被拒绝。
    let retry = store
        .enqueue_with_notify(
            "qq:msg:msg-1",
            "hello",
            "codex",
            ru41::domain::RunContract::AnalysisV1,
            &route,
        )
        .unwrap();
    assert!(!retry.created);
    let other = ru41::domain::NotifyRoute {
        channel: "qq".into(),
        target: "group:G2".into(),
        msg_id: Some("msg-1".into()),
    };
    assert!(
        store
            .enqueue_with_notify(
                "qq:msg:msg-1",
                "hello",
                "codex",
                ru41::domain::RunContract::AnalysisV1,
                &other,
            )
            .is_err()
    );
    // 运行中的任务不进入通知队列；完成后进入，标记后不重复。
    assert!(store.list_unnotified(10).unwrap().is_empty());
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    assert!(store.list_unnotified(10).unwrap().is_empty());
    store
        .succeed(
            &claim,
            &AgentResult {
                summary: "done".into(),
            },
            None,
            101,
        )
        .unwrap();
    let pending = store.list_unnotified(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, added.task.id);
    assert!(store.mark_notified(added.task.id).unwrap());
    assert!(!store.mark_notified(added.task.id).unwrap());
    assert!(store.list_unnotified(10).unwrap().is_empty());
    // 无通知路由的普通任务永远不会出现在通知队列里。
    let plain = store.enqueue("plain", "hello").unwrap().task;
    let claim = store.claim("worker", 102, 30).unwrap().unwrap();
    store
        .succeed(
            &claim,
            &AgentResult {
                summary: "done".into(),
            },
            None,
            103,
        )
        .unwrap();
    assert!(store.list_unnotified(10).unwrap().is_empty());
    assert!(store.get(plain.id).unwrap().unwrap().notify.is_none());
}
