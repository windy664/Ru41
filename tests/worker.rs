use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{AgentResult, TaskStatus},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor, BackendRegistry},
    ports::storage::TaskStore,
    runtime::worker::{Worker, WorkerConfig},
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::watch;

struct FakeAgent {
    delay: Duration,
    summary: &'static str,
}
impl AgentBackend for FakeAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex".into(),
            model: None,
            contracts: vec![ru41::domain::RunContract::AnalysisV1],
            supports_resume: false,
        }
    }
    fn run(
        &self,
        _: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            Ok(AgentOutput {
                result: AgentResult {
                    summary: self.summary.into(),
                },
                session_id: Some("test-thread".into()),
            })
        })
    }
}
fn worker(dir: &std::path::Path, delay: Duration, summary: &'static str) -> Worker {
    let mut backends = BackendRegistry::default();
    backends.register(FakeAgent { delay, summary }).unwrap();
    Worker {
        store: Arc::new(SqliteTaskStore::open(&dir.join("state.db")).unwrap()),
        backends: Arc::new(backends),
        sanitizer: Arc::new(ru41::privacy::RandomSanitizer::default()),
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 10,
            retry_delay_secs: 10,
        },
    }
}
#[tokio::test]
async fn validated_result_is_persisted_and_not_run_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::ZERO, "done");
    w.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = w.run_once(receiver.clone()).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    assert!(w.run_once(receiver).await.unwrap().is_none());
    let a = w.store.attempts(report.task_id).unwrap();
    assert!(a[0].run_dir.is_some());
    assert_eq!(a[0].result.as_ref().unwrap().summary, "done");
}
#[tokio::test]
async fn invalid_agent_result_is_not_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::ZERO, " ");
    w.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    assert_eq!(
        w.run_once(receiver).await.unwrap().unwrap().status,
        TaskStatus::RetryScheduled
    );
}
#[tokio::test]
async fn timeout_records_failure_and_schedules_retry() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::from_secs(60), "done");
    w.config.timeout = Duration::from_millis(30);
    let id = w.store.enqueue("event", "hello").unwrap().task.id;
    let (_sender, receiver) = watch::channel(false);
    assert_eq!(
        w.run_once(receiver).await.unwrap().unwrap().status,
        TaskStatus::RetryScheduled
    );
    assert!(
        w.store.attempts(id).unwrap()[0]
            .error
            .as_ref()
            .unwrap()
            .contains("timed out")
    );
}
#[tokio::test]
async fn shutdown_interrupts_inflight_run() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::from_secs(60), "done");
    let id = w.store.enqueue("event", "hello").unwrap().task.id;
    let (sender, receiver) = watch::channel(false);
    let stop = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        sender.send(true).unwrap();
    };
    let (report, ()) = tokio::join!(w.run_once(receiver), stop);
    assert_eq!(report.unwrap().unwrap().status, TaskStatus::RetryScheduled);
    assert_eq!(w.store.attempts(id).unwrap()[0].status, "interrupted");
}
#[tokio::test]
async fn external_cancellation_is_observed_before_saving_result() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::from_secs(60), "done");
    let id = w.store.enqueue("event", "hello").unwrap().task.id;
    let (_sender, receiver) = watch::channel(false);
    let path = dir.path().join("state.db");
    let cancel = async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        SqliteTaskStore::open(&path).unwrap().cancel(id).unwrap();
    };
    let (report, ()) = tokio::join!(w.run_once(receiver), cancel);
    assert_eq!(report.unwrap().unwrap().status, TaskStatus::Cancelled);
}
