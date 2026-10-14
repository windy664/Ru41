use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::consumers::{ConsumerSpec, EventDelivery},
    ports::{
        consumers::{ConsumerStore, EventHandler},
        storage::TaskStore,
    },
    runtime::consumers::{ConsumeOutcome, ConsumerWorker},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;

struct Handler {
    fail: AtomicBool,
    delay: Duration,
    keys: Mutex<Vec<String>>,
}
impl EventHandler for Handler {
    fn handle<'a>(
        &'a self,
        d: &'a EventDelivery,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.keys.lock().unwrap().push(d.idempotency_key());
            tokio::time::sleep(self.delay).await;
            anyhow::ensure!(
                !self.fail.swap(false, Ordering::SeqCst),
                "private handler error"
            );
            Ok(())
        })
    }
}
fn setup(
    delay: Duration,
    fail: bool,
) -> (
    tempfile::TempDir,
    Arc<SqliteTaskStore>,
    Arc<Handler>,
    ConsumerWorker,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    store.enqueue("event", "private").unwrap();
    let handler = Arc::new(Handler {
        fail: AtomicBool::new(fail),
        delay,
        keys: Mutex::new(Vec::new()),
    });
    let worker = ConsumerWorker {
        store: store.clone(),
        handler: handler.clone(),
        spec: ConsumerSpec {
            name: "test".into(),
            start_after: 0,
            max_attempts: 3,
        },
        lease_secs: 3,
        timeout: Duration::from_secs(10),
        retry_delay_secs: 0,
    };
    (dir, store, handler, worker)
}

#[tokio::test]
async fn failure_retries_same_key_then_advances_once() {
    let (_dir, store, handler, worker) = setup(Duration::ZERO, true);
    let (_tx, rx) = watch::channel(false);
    assert!(matches!(
        worker.run_once(rx.clone()).await.unwrap(),
        ConsumeOutcome::Failed(_)
    ));
    assert_eq!(
        store
            .consumer("test")
            .unwrap()
            .unwrap()
            .last_failure
            .as_deref(),
        Some("handler_failed")
    );
    assert!(matches!(
        worker.run_once(rx.clone()).await.unwrap(),
        ConsumeOutcome::Handled(_)
    ));
    assert_eq!(worker.run_once(rx).await.unwrap(), ConsumeOutcome::Idle);
    let keys = handler.keys.lock().unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    assert!(store.consumer("test").unwrap().unwrap().cursor > 0);
}

#[tokio::test]
async fn timeout_and_shutdown_do_not_acknowledge() {
    let (_dir, store, _handler, mut worker) = setup(Duration::from_secs(10), false);
    worker.timeout = Duration::from_millis(20);
    let (tx, rx) = watch::channel(false);
    assert!(matches!(
        worker.run_once(rx.clone()).await.unwrap(),
        ConsumeOutcome::Failed(_)
    ));
    assert_eq!(
        store
            .consumer("test")
            .unwrap()
            .unwrap()
            .last_failure
            .as_deref(),
        Some("timeout")
    );
    worker.timeout = Duration::from_secs(30);
    let (result, ()) = tokio::join!(worker.run_once(rx.clone()), async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        tx.send(true).unwrap();
    });
    assert!(matches!(result.unwrap(), ConsumeOutcome::Failed(_)));
    let state = store.consumer("test").unwrap().unwrap();
    assert_eq!(state.cursor, 0);
    assert_eq!(state.last_failure.as_deref(), Some("interrupted"));
    assert_eq!(worker.run_once(rx).await.unwrap(), ConsumeOutcome::Stopped);
}

#[tokio::test]
async fn long_handler_renews_lease() {
    let (_dir, store, _handler, worker) = setup(Duration::from_secs(4), false);
    let (_tx, rx) = watch::channel(false);
    let (result, ()) = tokio::join!(worker.run_once(rx), async {
        tokio::time::sleep(Duration::from_millis(3200)).await;
        assert!(
            store
                .claim_event("test", ru41::runtime::worker::now(), 3)
                .unwrap()
                .is_none()
        );
    });
    assert!(matches!(result.unwrap(), ConsumeOutcome::Handled(_)));
}

#[tokio::test]
async fn lease_takeover_stops_the_old_handler_without_acknowledging() {
    let (_dir, store, _handler, worker) = setup(Duration::from_secs(10), false);
    let (_tx, rx) = watch::channel(false);
    let (result, ()) = tokio::join!(worker.run_once(rx), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let new = store
            .claim_event("test", ru41::runtime::worker::now() + 10, 30)
            .unwrap()
            .unwrap();
        assert_eq!(new.attempt, 2);
    });
    assert!(matches!(result.unwrap(), ConsumeOutcome::LeaseLost(_)));
    assert_eq!(store.consumer("test").unwrap().unwrap().cursor, 0);
}
