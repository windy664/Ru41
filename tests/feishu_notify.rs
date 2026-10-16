use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{AgentResult, NotifyRoute, RunContract, TaskStatus},
    ingress::feishu::{DEFAULT_TARGET, FeishuWebhookNotifier},
    ports::channel::NotifyChannel,
    ports::storage::TaskStore,
    runtime::dispatcher::NotifyDispatcher,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

/// 本地 mock webhook：记录请求体并按给定 payload 应答。
fn mock_webhook(payload: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let bodies_clone = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            bodies_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, bodies)
}

#[tokio::test]
async fn sends_text_message_and_accepts_code_zero() {
    let (url, bodies) = mock_webhook(r#"{"code":0,"msg":"success"}"#);
    let notifier = FeishuWebhookNotifier::new(&url);
    notifier
        .send(DEFAULT_TARGET, None, "任务完成：摘要")
        .await
        .unwrap();
    let body = &bodies.lock().unwrap()[0];
    assert!(body.contains(r#""msg_type":"text"#));
    assert!(body.contains("任务完成：摘要"));
}

#[tokio::test]
async fn accepts_legacy_status_code_response() {
    let (url, _) = mock_webhook(r#"{"StatusCode":0,"StatusMessage":"success"}"#);
    let notifier = FeishuWebhookNotifier::new(&url);
    notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap();
}

#[tokio::test]
async fn nonzero_code_is_an_error() {
    let (url, _) = mock_webhook(r#"{"code":19021,"msg":"sign not match"}"#);
    let notifier = FeishuWebhookNotifier::new(&url);
    let error = notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap_err();
    assert!(format!("{error:#}").contains("19021"));
}

#[tokio::test]
async fn resolves_default_and_explicit_targets() {
    let (url, bodies) = mock_webhook(r#"{"code":0}"#);
    let notifier = FeishuWebhookNotifier::new(&url);
    // 显式 URL 直接投递；非法 target 拒绝。
    notifier.send(&url, None, "explicit").await.unwrap();
    assert!(notifier.send("not-a-url", None, "hi").await.is_err());
    assert_eq!(bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dispatcher_delivers_terminal_task_to_default_webhook() {
    let (url, bodies) = mock_webhook(r#"{"code":0,"msg":"success"}"#);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "cli:test:1",
            "分析一下这篇论文",
            "codex",
            RunContract::AnalysisV1,
            &NotifyRoute {
                channel: "feishu".into(),
                target: DEFAULT_TARGET.into(),
                msg_id: None,
            },
        )
        .unwrap()
        .task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store
        .succeed(
            &claim,
            &AgentResult {
                summary: "论文核心结论".into(),
            },
            None,
            101,
        )
        .unwrap();
    assert_eq!(
        store.get(task.id).unwrap().unwrap().status,
        TaskStatus::Succeeded
    );
    let dispatcher = NotifyDispatcher {
        store: store.clone(),
        channels: vec![Arc::new(FeishuWebhookNotifier::new(&url))],
        batch: 10,
        interval: Duration::from_secs(60),
    };
    assert_eq!(dispatcher.run_once().await.unwrap(), 1);
    assert_eq!(dispatcher.run_once().await.unwrap(), 0);
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("论文核心结论"));
    assert!(store.get(task.id).unwrap().unwrap().notified_at.is_some());
}
