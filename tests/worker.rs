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
                    ..Default::default()
                },
                session_id: Some("test-thread".into()),
                usage: None,
                model: None,
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
        knowledge: None,
        silent_sources: Vec::new(),
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
                    ..Default::default()
                },
                session_id: Some("t1".into()),
                usage: None,
                model: None,
            })
        })
    }
}

#[tokio::test]
async fn agent_events_are_streamed_to_activity_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = worker_with(dir.path(), EventfulAgent);
    let _enqueued = w.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = w.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    let run_id = format!("run-{}-{}", report.task_id, report.attempt_id);
    // 停止信号后 streamer 会 flush 剩余行；稍等一拍再查。
    for _ in 0..50 {
        let docs = w.store.documents("activity", 10).unwrap();
        let mine: Vec<_> = docs.iter().filter(|d| d.id.starts_with(&run_id)).collect();
        let all_sent = !mine.is_empty() && mine.iter().all(|d| d.value["status"] == "pending");
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
    // 输出不合契约是采样问题：判瞬态、退避重排，不在第一次就判死。
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

/// 总是以固定错误失败的底座。
struct FailingAgent {
    error: fn() -> anyhow::Error,
}
impl AgentBackend for FailingAgent {
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
        Box::pin(async move { Err((self.error)()) })
    }
}

/// 装一个总是失败的底座；retry_delay=0 让重排的任务立刻可被再次认领。
fn failing_worker(
    dir: &std::path::Path,
    error: fn() -> anyhow::Error,
    retry_delay_secs: i64,
) -> Worker {
    let mut backends = BackendRegistry::default();
    backends.register(FailingAgent { error }).unwrap();
    Worker {
        silent_sources: Vec::new(),
        store: Arc::new(SqliteTaskStore::open(&dir.join("state.db")).unwrap()),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        knowledge: None,
        config: WorkerConfig {
            worker_id: "test".into(),
            runs_dir: dir.join("runs"),
            timeout: Duration::from_secs(2),
            lease_secs: 60,
            retry_delay_secs,
        },
    }
}

#[tokio::test]
async fn permanent_failure_skips_retry_budget() {
    // 配置错误重试也不会变：直接终态 failed，不进 retry_scheduled。
    let dir = tempfile::tempdir().unwrap();
    let mut worker = failing_worker(
        dir.path(),
        || ru41::domain::error::Ru41Error::Config("bad config".into()).into(),
        10,
    );
    worker.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = worker.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Failed);
    let task = worker.store.get(report.task_id).unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.attempt_count, 1);
    assert!(
        task.last_error
            .as_deref()
            .unwrap()
            .contains("invalid configuration")
    );
}

#[tokio::test]
async fn invalid_output_retries_until_budget_exhausted() {
    // 输出不合契约：先重排重试，预算耗尽才放弃——不是第一次就判死。
    let dir = tempfile::tempdir().unwrap();
    let mut worker = failing_worker(
        dir.path(),
        || ru41::domain::error::Ru41Error::InvalidOutput("not json".into()).into(),
        0,
    );
    worker.store.enqueue("event", "hello").unwrap();
    let (_sender, receiver) = watch::channel(false);
    let mut statuses = Vec::new();
    let mut task_id = 0;
    for _ in 0..4 {
        let Some(report) = worker.run_once(receiver.clone()).await.unwrap() else {
            break;
        };
        task_id = report.task_id;
        statuses.push(report.status);
        if report.status == TaskStatus::Failed {
            break;
        }
    }
    assert_eq!(
        statuses,
        vec![
            TaskStatus::RetryScheduled,
            TaskStatus::RetryScheduled,
            TaskStatus::Failed
        ],
        "连续不合契约应先重试，耗尽预算才放弃：{statuses:?}"
    );
    let task = worker.store.get(task_id).unwrap().unwrap();
    assert_eq!(task.attempt_count, 3);
    assert!(
        task.last_error
            .as_deref()
            .unwrap()
            .contains("invalid agent output")
    );
}

#[test]
fn classify_sees_through_trace_context() {
    // 失败路径会给错误加一层「它做了什么：…」上下文（worker.rs 的 error.context）。
    // 分类必须能穿透这层，否则瞬态/永久的判定会整体退化成默认瞬态。
    let transient = anyhow::Error::new(ru41::domain::error::Ru41Error::InvalidOutput("bad".into()))
        .context("它做了什么：\n- bash");
    assert_eq!(
        ru41::domain::error::classify(&transient),
        ru41::domain::error::FailureKind::Transient
    );
    let permanent = anyhow::Error::new(ru41::domain::error::Ru41Error::Config("bad".into()))
        .context("它做了什么：\n- bash");
    assert_eq!(
        ru41::domain::error::classify(&permanent),
        ru41::domain::error::FailureKind::Permanent
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
                        ..Default::default()
                    },
                    session_id: Some("thread-1".into()),
                    usage: None,
                    model: None,
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
        silent_sources: Vec::new(),
        store: store.clone(),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        knowledge: None,
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
    for key in ["qq:msg:first", "qq:msg:second"] {
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

    // 后台推送任务（论文日报等）不占用聊天会话：没有会话键，也不该续接。
    store
        .enqueue_with_notify(
            "arxiv:digest:2026-11-12:1-40",
            "digest",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap();
    let report = worker.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    let last = requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.0, None, "后台任务不该拿到会话键");
    assert_eq!(last.1, None, "后台任务不该续接聊天会话");
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
                        ..Default::default()
                    },
                    session_id: Some("thread-glm".into()),
                    usage: None,
                    model: None,
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
        silent_sources: Vec::new(),
        store: store.clone(),
        backends: Arc::new(backends),
        sanitizer: Arc::new(NoopSanitizer),
        knowledge: None,
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
        .enqueue_with_notify(
            "qq:msg:m1",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
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
        .enqueue_with_notify(
            "qq:msg:m2",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap();
    let report = worker.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    assert_eq!(
        *requests.lock().unwrap(),
        vec![None, Some("thread-glm".into())]
    );
}

/// 恒定向量假嵌入：所有文本相似度恒为 1，检索必命中，专注测注入链路而非语义。
struct ConstantEmbedder;
impl ru41::ports::embedding::Embedder for ConstantEmbedder {
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0, 0.0]).collect())
    }
    fn dim(&self) -> usize {
        4
    }
    fn model_name(&self) -> &str {
        "constant-test"
    }
}

/// 回显收到的 prompt（summary 同样回显），供断言注入块的脱敏与回填。
struct EchoAgent(Arc<std::sync::Mutex<String>>);
impl AgentBackend for EchoAgent {
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
        let seen = self.0.clone();
        Box::pin(async move {
            *seen.lock().unwrap() = request.prompt.clone();
            Ok(AgentOutput {
                result: AgentResult {
                    summary: request.prompt.clone(),
                    ..Default::default()
                },
                session_id: Some("test-thread".into()),
                usage: None,
                model: None,
            })
        })
    }
}

#[tokio::test]
async fn output_style_rule_reaches_every_backend() {
    // 输出规范统一在 Worker 注入：Codex 与 Pi 拿到的必须是同一份
    // （数学公式一律 LaTeX）。
    let dir = tempfile::tempdir().unwrap();
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let mut w = worker_with(dir.path(), EchoAgent(seen.clone()));
    w.store
        .enqueue("event", "推导一下泰勒展开的余项估计")
        .unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = w.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);
    let prompt = seen.lock().unwrap().clone();
    assert!(prompt.contains("LaTeX"), "prompt: {prompt}");
    assert!(prompt.contains("$...$"), "prompt: {prompt}");
    assert!(prompt.contains("$$...$$"), "prompt: {prompt}");
}

#[tokio::test]
async fn knowledge_injection_passes_sanitizer_and_restores() {
    let dir = tempfile::tempdir().unwrap();
    let vault = tempfile::tempdir().unwrap();
    std::fs::write(
        vault.path().join("credentials.md"),
        "# 凭据\n\n数据库 password=Sup3rSecret99 仅自用\n",
    )
    .unwrap();
    let knowledge = Arc::new(
        ru41::app::knowledge::KnowledgeService::new(
            Arc::new(ConstantEmbedder),
            dir.path().join("knowledge.db"),
        )
        .unwrap(),
    );
    let indexed = knowledge.index_vault(vault.path()).unwrap();
    assert!(indexed.chunks >= 1);

    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let mut w = worker_with(dir.path(), EchoAgent(seen.clone()));
    w.knowledge = Some(knowledge);
    w.store
        .enqueue("event", "帮我看一下笔记里记录的数据库密码是多少")
        .unwrap();
    let (_sender, receiver) = watch::channel(false);
    let report = w.run_once(receiver).await.unwrap().unwrap();
    assert_eq!(report.status, TaskStatus::Succeeded);

    let prompt = seen.lock().unwrap().clone();
    // 命中注入进了 prompt，但真密码不得出站——检索块也必须过脱敏。
    assert!(prompt.contains("知识库自动检索结果"), "prompt: {prompt}");
    assert!(!prompt.contains("Sup3rSecret99"), "prompt: {prompt}");
    assert!(prompt.contains("key_"), "prompt: {prompt}");
    // 回复里的假值经合并后的映射回填为真值，落库与通知是真实内容。
    let attempts = w.store.attempts(report.task_id).unwrap();
    let summary = &attempts[0].result.as_ref().unwrap().summary;
    assert!(summary.contains("Sup3rSecret99"), "summary: {summary}");
}
