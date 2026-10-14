use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{AgentResult, RunContract, TaskStatus},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor, BackendRegistry},
    ports::storage::TaskStore,
    runtime::worker::{Worker, WorkerConfig},
};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::watch;

struct TestBackend(&'static str);
impl AgentBackend for TestBackend {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: self.0.into(),
            model: Some("test-model".into()),
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: false,
        }
    }
    fn run(
        &self,
        _: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
        Box::pin(async {
            Ok(AgentOutput {
                result: AgentResult {
                    summary: format!("handled by {}", self.0),
                },
                session_id: Some("opaque-session".into()),
            })
        })
    }
}

#[tokio::test]
async fn two_backends_share_worker_but_never_take_each_others_tasks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let store = SqliteTaskStore::open(&path).unwrap();
    let first = store
        .enqueue_for("one", "hello", "alpha", RunContract::AnalysisV1)
        .unwrap()
        .task
        .id;
    let second = store
        .enqueue_for("two", "hello", "beta", RunContract::AnalysisV1)
        .unwrap()
        .task
        .id;
    assert!(
        store
            .enqueue_for("one", "hello", "beta", RunContract::AnalysisV1)
            .is_err()
    );
    let mut registry = BackendRegistry::default();
    registry.register(TestBackend("alpha")).unwrap();
    registry.register(TestBackend("beta")).unwrap();
    assert!(registry.register(TestBackend("alpha")).is_err());
    assert!(registry.get("missing").is_err());
    let (_sender, receiver) = watch::channel(false);
    let mut worker = Worker {
        store: Arc::new(SqliteTaskStore::open(&path).unwrap()),
        backends: Arc::new(registry),
        sanitizer: Arc::new(ru41::privacy::RandomSanitizer::default()),
        config: WorkerConfig {
            worker_id: "shared-worker".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(1),
            lease_secs: 10,
            retry_delay_secs: 1,
        },
    };
    // 同一个 Worker 按任务绑定的 backend_id 路由到对应底座。
    for (backend, id) in [("alpha", first), ("beta", second)] {
        let report = worker.run_once(receiver.clone()).await.unwrap().unwrap();
        assert_eq!(report.task_id, id);
        assert_eq!(report.status, TaskStatus::Succeeded);
        let history = store.attempts(id).unwrap();
        assert_eq!(history[0].backend_id, backend);
        assert_eq!(history[0].model.as_deref(), Some("test-model"));
        assert_eq!(history[0].session_id.as_deref(), Some("opaque-session"));
    }
    assert!(worker.run_once(receiver.clone()).await.unwrap().is_none());
}

#[test]
fn v2_migration_preserves_codex_session_and_assigns_backend() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(include_str!("../migrations/001_tasks.sql"))
        .unwrap();
    conn.execute_batch(include_str!("../migrations/002_execution.sql"))
        .unwrap();
    conn.execute(
        "INSERT INTO tasks(id,request_key,prompt,status) VALUES(1,'old','hello','succeeded')",
        [],
    )
    .unwrap();
    conn.execute("INSERT INTO attempts(task_id,worker_id,status,started_at,thread_id) VALUES(1,'old','succeeded',1,'codex-thread')", []).unwrap();
    drop(conn);
    let store = SqliteTaskStore::open(&path).unwrap();
    assert_eq!(store.get(1).unwrap().unwrap().backend_id, "codex");
    let attempts = store.attempts(1).unwrap();
    assert_eq!(attempts[0].session_id.as_deref(), Some("codex-thread"));
    assert_eq!(attempts[0].contract, RunContract::AnalysisV1);
}
