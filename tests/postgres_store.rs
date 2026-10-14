use ru41::{
    adapters::database::postgres::PostgresTaskStore,
    domain::{AgentResult, TaskStatus},
    ports::storage::TaskStore,
};
use std::sync::{Arc, Barrier, Mutex, MutexGuard};

// 测试共用一个数据库，串行执行避免互相清表。
static DB_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    DB_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 未设置 RU41_TEST_PG_URL 时跳过；设置时真实连库并清空表。
fn store() -> Option<PostgresTaskStore> {
    let Ok(url) = std::env::var("RU41_TEST_PG_URL") else {
        eprintln!("RU41_TEST_PG_URL is not set; skipping PostgreSQL store test");
        return None;
    };
    let mut client = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    client
        .batch_execute("DROP TABLE IF EXISTS schema_migrations, attempts, tasks, records, events, event_clock, event_consumers CASCADE")
        .unwrap();
    drop(client);
    Some(PostgresTaskStore::open(&url).unwrap())
}

#[test]
fn multiple_workers_cannot_claim_the_same_task() {
    let _guard = lock();
    let Some(store) = store() else { return };
    store.enqueue("event", "hello").unwrap();
    let url = std::env::var("RU41_TEST_PG_URL").unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let url = url.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = PostgresTaskStore::open(&url).unwrap();
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
    let _guard = lock();
    let Some(a) = store() else { return };
    let url = std::env::var("RU41_TEST_PG_URL").unwrap();
    a.enqueue("event", "hello").unwrap();
    let old = a.claim("old", 100, 10).unwrap().unwrap();
    assert!(!a.heartbeat(&old, 110, 10).unwrap());
    let b = PostgresTaskStore::open(&url).unwrap();
    let new = b.claim("new", 111, 30).unwrap().unwrap();
    let result = AgentResult {
        summary: "done".into(),
    };
    assert!(!a.succeed(&old, &result, None, 112).unwrap());
    assert!(!a.fail(&old, "late failure", 112, 10, false, true).unwrap());
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
    let _guard = lock();
    let Some(store) = store() else { return };
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
    let _guard = lock();
    let Some(store) = store() else { return };
    let id = store.enqueue("event", "hello").unwrap().task.id;
    for i in 0..3 {
        let now = 100 + i * 20;
        let claim = store.claim("worker", now, 30).unwrap().unwrap();
        store
            .fail(&claim, "offline", now + 1, 10, false, true)
            .unwrap();
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
fn notify_route_roundtrip_and_mark_once() {
    let _guard = lock();
    let Some(store) = store() else { return };
    let route = ru41::domain::NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("msg-9".into()),
    };
    let added = store
        .enqueue_with_notify(
            "qq:msg:msg-9",
            "hello",
            "codex",
            ru41::domain::RunContract::AnalysisV1,
            &route,
        )
        .unwrap();
    assert!(added.created);
    assert_eq!(added.task.notify.as_ref(), Some(&route));
    assert!(store.list_unnotified(10).unwrap().is_empty());
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store.fail(&claim, "offline", 101, 0, false, true).unwrap();
    // 预算耗尽进 failed 后应出现在通知队列。
    for i in 1..3 {
        let now = 100 + i * 20;
        let claim = store.claim("worker", now, 30).unwrap().unwrap();
        store
            .fail(&claim, "offline", now + 1, 0, false, true)
            .unwrap();
    }
    let claim = store.claim("worker", 500, 30).unwrap();
    assert!(claim.is_none());
    let pending = store.list_unnotified(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].status, TaskStatus::Failed);
    assert!(store.mark_notified(added.task.id).unwrap());
    assert!(!store.mark_notified(added.task.id).unwrap());
    assert!(store.list_unnotified(10).unwrap().is_empty());
}

#[test]
fn interaction_reply_is_fenced_by_current_attempt() {
    let _guard = lock();
    let Some(store) = store() else { return };
    let task = store.enqueue("approval", "hello").unwrap().task;
    let old = store.claim("old", 100, 10).unwrap().unwrap();
    let request = ru41::domain::records::Interaction {
        id: "interaction".into(),
        run_id: "run".into(),
        request_id: serde_json::json!(1),
        method: "approval".into(),
        detail: serde_json::json!({}),
        options: vec![serde_json::json!("yes")],
        expires_at: 200,
        status: "pending".into(),
        response: None,
    };
    store
        .put_document(
            "run",
            "run",
            None,
            &serde_json::json!({"task_id":task.id,"attempt_id":old.attempt_id,"status":"running"}),
        )
        .unwrap();
    store
        .put_document(
            "interaction",
            "interaction",
            None,
            &serde_json::to_value(&request).unwrap(),
        )
        .unwrap();
    assert!(
        !store
            .reply_interaction("interaction", &serde_json::json!("invalid"), 101)
            .unwrap()
    );
    assert!(
        store
            .reply_interaction("interaction", &serde_json::json!("yes"), 101)
            .unwrap()
    );
    assert!(
        !store
            .reply_interaction("interaction", &serde_json::json!("yes"), 101)
            .unwrap()
    );
    store
        .put_document(
            "interaction",
            "interaction-2",
            None,
            &serde_json::to_value(&request).unwrap(),
        )
        .unwrap();
    store.claim("new", 111, 30).unwrap().unwrap();
    assert!(
        !store
            .reply_interaction("interaction-2", &serde_json::json!("yes"), 112)
            .unwrap()
    );
}

#[path = "common/event_contract.rs"]
mod event_contract;

#[test]
fn persistent_events_follow_the_shared_contract() {
    let _guard = lock();
    let Some(store) = store() else { return };
    event_contract::assert_event_contract(&store);
}

#[test]
fn event_cursor_follows_commit_order_and_rollback() {
    use ru41::ports::events::EventStore;
    use std::{sync::mpsc, time::Duration};
    let _guard = lock();
    let Some(store) = store() else { return };
    let url = std::env::var("RU41_TEST_PG_URL").unwrap();
    let mut first = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let mut tx = first.transaction().unwrap();
    tx.execute(
        "INSERT INTO tasks(request_key,prompt) VALUES('first','private')",
        &[],
    )
    .unwrap();
    assert_eq!(store.event_cursor().unwrap(), 0);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (inserted_tx, inserted_rx) = mpsc::channel();
    let (commit_tx, commit_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut second = postgres::Client::connect(&url, postgres::NoTls).unwrap();
        let mut tx = second.transaction().unwrap();
        ready_tx.send(()).unwrap();
        tx.execute(
            "INSERT INTO tasks(request_key,prompt) VALUES('second','private')",
            &[],
        )
        .unwrap();
        inserted_tx.send(()).unwrap();
        commit_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        tx.commit().unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(matches!(
        inserted_rx.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    tx.commit().unwrap();
    inserted_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let cursor = store.event_cursor().unwrap();
    assert_eq!(store.events_after(0, 100).unwrap().len(), 1);
    commit_tx.send(()).unwrap();
    thread.join().unwrap();
    assert_eq!(store.events_after(cursor, 100).unwrap().len(), 1);
    let before = store.event_cursor().unwrap();
    let mut tx = first.transaction().unwrap();
    tx.execute(
        "INSERT INTO tasks(request_key,prompt) VALUES('rolledback','private')",
        &[],
    )
    .unwrap();
    tx.rollback().unwrap();
    assert_eq!(store.event_cursor().unwrap(), before);
    assert!(store.events_after(before, 100).unwrap().is_empty());
}

#[path = "common/consumer_contract.rs"]
mod consumer_contract;

#[test]
fn postgres_consumer_contract() {
    let _guard = lock();
    let Some(store) = store() else { return };
    consumer_contract::assert_consumer_contract(&store);
}

#[test]
fn concurrent_consumers_share_one_lease() {
    use ru41::{domain::consumers::ConsumerSpec, ports::consumers::ConsumerStore};
    let _guard = lock();
    let Some(store) = store() else { return };
    store.enqueue("event", "x").unwrap();
    store
        .register_consumer(&ConsumerSpec {
            name: "shared".into(),
            start_after: 0,
            max_attempts: 3,
        })
        .unwrap();
    let url = std::env::var("RU41_TEST_PG_URL").unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let url = url.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let store = PostgresTaskStore::open(&url).unwrap();
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
