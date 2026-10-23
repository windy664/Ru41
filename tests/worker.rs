use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{AgentResult, RunContract, TaskStatus},
    ports::agent::{AgentBackend, AgentOutput, AgentRequest, BackendDescriptor, BackendRegistry},
    ports::storage::TaskStore,
    privacy::NoopSanitizer,
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
    worker_with(dir, FakeAgent { delay, summary })
}

fn worker_with(dir: &std::path::Path, agent: impl AgentBackend + 'static) -> Worker {
    let mut backends = BackendRegistry::default();
    backends.register(agent).unwrap();
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

/// 写 codex 风格 events.jsonl 后延迟完成的底座，验证活动流被翻译进 store。
struct EventfulAgent;
impl AgentBackend for EventfulAgent {
    fn descriptor(&self) -> BackendDescriptor {
        BackendDescriptor {
            id: "codex".into(),
            model: None,
            contracts: vec![RunContract::AnalysisV1],
            supports_resume: false,
        }
    }
    fn run(
        &self,
        request: AgentRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
        Box::pin(async move {
            let events = request.run_dir.join("events.jsonl");
            tokio::fs::write(
                &events,
                concat!(
                    r#"{"type":"thread.started","thread_id":"t1"}"#,
                    "\n",
                    r#"{"type":"item.completed","item":{"type":"reasoning","summary":"先分析"}}"#,
                    "\n",
                    r#"{"type":"item.completed","item":{"type":"agent_message","text":"我看完了"}}"#,
                    "\n",
                    r#"{"type":"item.completed","item":{"type":"local_shell_call","action":{"command":"ls"}}}"#,
                    "\n",
                    r#"{"type":"turn.completed"}"#,
                    "\n"
                ),
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(AgentOutput {
                result: AgentResult {
                    summary: "done".into(),
                },
                session_id: Some("t1".into()),
            })
        })
    }
}

#[tokio::test]
async fn agent_events_are_streamed_to_activity_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker_with(dir.path(), EventfulAgent);
    let enqueued = w.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = w.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    let run_id = format!("run-{}-{}", report.task_id, report.attempt_id);
    // 停止信号后 streamer 会 flush 剩余行；稍等一拍再查。
    for _ in 0..50 {
        let docs = w.store.documents("activity", 10).unwrap();
        let mine: Vec<_> = docs
            .iter()
            .filter(|d| d.id.starts_with(&run_id))
            .collect();
        let all_sent = !mine.is_empty()
            && mine.iter().all(|d| d.value["status"] == "pending");
        if all_sent {
            let lines: Vec<&str> = mine[0].value["lines"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            let text = lines.join("\n");
            assert!(text.contains("💭 先分析"), "chunk: {text}");
            assert!(text.contains("💬 我看完了"), "chunk: {text}");
            assert!(text.contains("⚙️ $ ls"), "chunk: {text}");
            assert!(
                !text.contains("turn.completed"),
                "无关事件不应进展示行: {text}"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("no activity chunk written for {run_id}");
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
    let run = w
        .store
        .document(
            "run",
            &format!("run-{}-{}", report.task_id, report.attempt_id),
        )
        .unwrap()
        .unwrap();
    assert_eq!(run.value["session_id"], "test-thread");
    assert_eq!(run.value["status"], "succeeded");
}
#[tokio::test]
async fn invalid_agent_result_is_not_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker(dir.path(), Duration::ZERO, " ");
    w.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    assert_eq!(
        w.run_once(receiver).await.unwrap().unwrap().status,
        TaskStatus::Failed
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

#[tokio::test]
async fn permanent_failure_skips_retry_budget() {
    // 非法输出是永久失败：不进 retry_scheduled，直接终态 failed。
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    struct BadOutput;
    impl AgentBackend for BadOutput {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "codex".into(),
                model: None,
                contracts: vec![RunContract::AnalysisV1],
                supports_resume: false,
            }
        }
        fn run(
            &self,
            _request: AgentRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
            Box::pin(async {
                Err(ru41::domain::error::Ru41Error::InvalidOutput("not json".into()).into())
            })
        }
    }
    let mut backends = BackendRegistry::default();
    backends.register(BadOutput).unwrap();
    let mut worker = Worker {
        store: store.clone(),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 60,
            retry_delay_secs: 10,
        },
    };
    store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = worker.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Failed);
    let task = store.get(report.task_id).unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert!(
        task.last_error
            .as_deref()
            .unwrap()
            .contains("invalid agent output")
    );
}

#[tokio::test]
async fn conversation_session_is_saved_and_resumed() {
    type Requests = Arc<std::sync::Mutex<Vec<(Option<String>, Option<String>)>>>;
    struct ConversationAgent {
        requests: Requests,
        model: Option<String>,
    }
    impl AgentBackend for ConversationAgent {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "codex".into(),
                model: self.model.clone(),
                contracts: vec![RunContract::AnalysisV1],
                supports_resume: true,
            }
        }
        fn run(
            &self,
            request: AgentRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
            let context = request.context.expect("conversation context");
            assert_eq!(context.skills.len(), 1);
            assert_eq!(context.skills[0].name, "test-skill");
            self.requests
                .lock()
                .unwrap()
                .push((context.conversation_id, context.resume_session_id));
            Box::pin(async {
                Ok(AgentOutput {
                    result: AgentResult {
                        summary: "done".into(),
                    },
                    session_id: Some("thread-1".into()),
                })
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut backends = BackendRegistry::default();
    backends
        .register(ConversationAgent {
            requests: requests.clone(),
            model: None,
        })
        .unwrap();
    backends.skills = vec![ru41::skills::SkillPackage {
        name: "test-skill".into(),
        description: "Test skill".into(),
        instructions: "Follow the test workflow".into(),
        files: vec![],
    }];
    let mut worker = Worker {
        store: store.clone(),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 60,
            retry_delay_secs: 10,
        },
    };
    let route = ru41::domain::NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: None,
    };
    let (_sender, receiver) = watch::channel(false);
    for key in ["first", "second"] {
        store
            .enqueue_with_notify(key, "hello", "codex", RunContract::AnalysisV1, &route)
            .unwrap();
        let report = worker.run_once(receiver.clone()).await.unwrap().unwrap();
        assert_eq!(report.status, TaskStatus::Succeeded);
    }

    let document = store
        .document("conversation", "qq:user:U1")
        .unwrap()
        .unwrap();
    assert_eq!(document.value["session_id"], "thread-1");
    assert_eq!(
        *requests.lock().unwrap(),
        vec![
            (Some("qq:user:U1".into()), None),
            (Some("qq:user:U1".into()), Some("thread-1".into())),
        ]
    );
}

/// 会话续接按底座+模型匹配：换了模型（如 mimo → glm）后不得续接旧会话。
#[tokio::test]
async fn conversation_resume_requires_matching_model() {
    struct ConversationAgent {
        requests: Arc<std::sync::Mutex<Vec<Option<String>>>>,
    }
    impl AgentBackend for ConversationAgent {
        fn descriptor(&self) -> BackendDescriptor {
            BackendDescriptor {
                id: "codex".into(),
                model: Some("glm-4.6".into()),
                contracts: vec![RunContract::AnalysisV1],
                supports_resume: true,
            }
        }
        fn run(
            &self,
            request: AgentRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<AgentOutput>> + Send + '_>> {
            let context = request.context.expect("conversation context");
            self.requests
                .lock()
                .unwrap()
                .push(context.resume_session_id);
            Box::pin(async {
                Ok(AgentOutput {
                    result: AgentResult {
                        summary: "done".into(),
                    },
                    session_id: Some("thread-glm".into()),
                })
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut backends = BackendRegistry::default();
    backends
        .register(ConversationAgent {
            requests: requests.clone(),
        })
        .unwrap();
    let mut worker = Worker {
        store: store.clone(),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.path().join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 60,
            retry_delay_secs: 10,
        },
    };
    let route = ru41::domain::NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: None,
    };
    // 预置 mimo 时代的旧会话文档（无 model 字段）。
    store
        .put_document(
            "conversation",
            "qq:user:U1",
            None,
            &serde_json::json!({"backend_id": "codex", "session_id": "thread-mimo"}),
        )
        .unwrap();
    let (_sender, receiver) = watch::channel(false);
    store
        .enqueue_with_notify("m1", "hello", "codex", RunContract::AnalysisV1, &route)
        .unwrap();
    let report = worker.run_once(receiver.clone()).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    // 旧会话模型不匹配：不得续接，且成功后写回新模型与会话。
    assert_eq!(*requests.lock().unwrap(), vec![None]);
    let document = store
        .document("conversation", "qq:user:U1")
        .unwrap()
        .unwrap();
    assert_eq!(document.value["session_id"], "thread-glm");
    assert_eq!(document.value["model"], "glm-4.6");
    // 同模型第二次任务正常续接。
    store
        .enqueue_with_notify("m2", "hello", "codex", RunContract::AnalysisV1, &route)
        .unwrap();
    let report = worker.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    assert_eq!(
        *requests.lock().unwrap(),
        vec![None, Some("thread-glm".into())]
    );
}
