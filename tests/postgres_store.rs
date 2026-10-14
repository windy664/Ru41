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
        .batch_execute("DROP TABLE IF EXISTS schema_migrations, attempts, tasks, records CASCADE")
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
