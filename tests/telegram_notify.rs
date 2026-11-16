use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    channels::telegram::{DEFAULT_TARGET, TelegramNotifier, api::TelegramApi},
    domain::{AgentResult, NotifyRoute, RunContract, TaskStatus},
    ports::channel::{MediaKind, MediaRef, NotifyChannel},
    ports::storage::TaskStore,
    runtime::dispatcher::NotifyDispatcher,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::Duration,
};

const TOKEN: &str = "123456:test-token-value";

/// mock Bot API 收到的一条请求。
#[derive(Clone, Debug)]
struct RecordedRequest {
    path: String,
    body: String,
}

/// 本地 mock Bot API：按队列依次应答，并记录请求路径与请求体。
fn mock_bot(responses: Vec<&'static str>) -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&recorded);
    let responses: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(responses));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut path = String::new();
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                if path.is_empty() {
                    // 请求行：METHOD path HTTP/1.1
                    let mut parts = line.split_whitespace();
                    let _method = parts.next();
                    path = parts.next().unwrap_or_default().to_owned();
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
            recorder.lock().unwrap().push(RecordedRequest {
                path,
                body: String::from_utf8_lossy(&body).into_owned(),
            });
            let payload = {
                let mut queue = responses.lock().unwrap();
                if queue.is_empty() {
                    r#"{"ok":true,"result":{"id":1,"username":"ru41bot"}}"#
                } else {
                    queue.remove(0)
                }
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, recorded)
}

fn notifier(base: &str, default_chat: Option<&str>) -> TelegramNotifier {
    TelegramNotifier::new(
        Arc::new(TelegramApi::with_base(TOKEN, base)),
        default_chat.map(str::to_owned),
    )
}

#[tokio::test]
async fn sends_html_formatted_text_to_configured_chat() {
    let (base, recorded) = mock_bot(vec![r#"{"ok":true,"result":{"message_id":7}}"#]);
    let notifier = notifier(&base, Some("998877"));
    notifier
        .send(DEFAULT_TARGET, None, "## 任务完成\n**重点**：已生成")
        .await
        .unwrap();
    let request = &recorded.lock().unwrap()[0];
    assert!(request.path.ends_with("/sendMessage"));
    assert!(request.body.contains(r#""parse_mode":"HTML""#));
    assert!(request.body.contains("<b>任务完成</b>"));
    assert!(request.body.contains(r#""chat_id":"998877""#));
}

#[tokio::test]
async fn resolves_explicit_chat_targets_and_rejects_garbage() {
    let (base, recorded) = mock_bot(vec![]);
    let notifier = notifier(&base, Some("998877"));
    notifier.send("chat:123", None, "hi").await.unwrap();
    notifier.send("@ru41bot", None, "hi").await.unwrap();
    assert!(notifier.send("not-a-chat", None, "hi").await.is_err());
    assert_eq!(recorded.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn missing_default_chat_is_a_clear_error() {
    let (base, _) = mock_bot(vec![]);
    let notifier = notifier(&base, None);
    let error = notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap_err();
    assert!(format!("{error:#}").contains("default_chat_id"));
}

#[tokio::test]
async fn platform_error_never_leaks_token() {
    let (base, _) = mock_bot(vec![
        r#"{"ok":false,"error_code":400,"description":"Bad Request: chat not found"}"#,
    ]);
    let notifier = notifier(&base, Some("998877"));
    let error = notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("chat not found"));
    assert!(!text.contains(TOKEN), "token leaked: {text}");
}

#[tokio::test]
async fn parse_failure_falls_back_to_plain_text() {
    let (base, recorded) = mock_bot(vec![
        r#"{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities"}"#,
        r#"{"ok":true,"result":{"message_id":8}}"#,
    ]);
    let notifier = notifier(&base, Some("998877"));
    notifier
        .send(DEFAULT_TARGET, None, "**带强调**的文本")
        .await
        .unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert!(recorded[0].body.contains(r#""parse_mode":"HTML""#));
    assert!(!recorded[1].body.contains("parse_mode"));
    assert!(recorded[1].body.contains("带强调"));
}

#[tokio::test]
async fn sends_remote_media_as_photo() {
    let (base, recorded) = mock_bot(vec![r#"{"ok":true,"result":{"message_id":9}}"#]);
    let notifier = notifier(&base, Some("998877"));
    let media = MediaRef::new(MediaKind::Image, "https://example.com/a.png");
    notifier
        .send_media(DEFAULT_TARGET, None, "截图", &media)
        .await
        .unwrap();
    let request = &recorded.lock().unwrap()[0];
    assert!(request.path.ends_with("/sendPhoto"));
    assert!(
        request
            .body
            .contains(r#""photo":"https://example.com/a.png""#)
    );
    assert!(request.body.contains(r#""caption":"截图""#));
}

#[tokio::test]
async fn dispatcher_delivers_terminal_task_to_telegram() {
    let (base, recorded) = mock_bot(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "cli:test:telegram",
            "整理这篇文章",
            "codex",
            RunContract::AnalysisV1,
            &NotifyRoute {
                channel: "telegram".into(),
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
                ..Default::default()
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
        silent_sources: Vec::new(),
        fanout: Vec::new(),
        store: store.clone(),
        channels: vec![Arc::new(notifier(&base, Some("998877")))],
        batch: 10,
        interval: Duration::from_secs(60),
    };
    assert_eq!(dispatcher.run_once().await.unwrap(), 1);
    assert_eq!(dispatcher.run_once().await.unwrap(), 0);
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(recorded[0].body.contains("论文核心结论"));
    assert!(store.get(task.id).unwrap().unwrap().notified_at.is_some());
}

/// 限速（429）短等待：就地退避后重试，消息不丢。
#[tokio::test]
async fn flood_control_is_retried_in_place() {
    let (base, recorded) = mock_bot(vec![
        r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 1","parameters":{"retry_after":1}}"#,
        r#"{"ok":true,"result":{"message_id":11}}"#,
    ]);
    let notifier = notifier(&base, Some("900005"));
    notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 2, "限速后应重试一次");
    assert!(recorded[1].body.contains("hi"));
}

/// 限速等待过长：不就地睡（网关是长驻进程），把失败交给投递层改期重发。
#[tokio::test]
async fn long_flood_wait_is_not_slept_inline() {
    let (base, recorded) = mock_bot(vec![
        r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 30","parameters":{"retry_after":30}}"#,
    ]);
    let notifier = notifier(&base, Some("900003"));
    let started = std::time::Instant::now();
    let error = notifier.send(DEFAULT_TARGET, None, "hi").await.unwrap_err();
    assert!(format!("{error:#}").contains("429"), "{error:#}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "不该就地等 30 秒：{:?}",
        started.elapsed()
    );
    assert_eq!(recorded.lock().unwrap().len(), 1);
}

/// 同一会话连发要排队：Telegram 对单会话有速率上限，连发会被 429 拒收（消息直接丢）。
#[tokio::test]
async fn same_chat_messages_are_spaced_out() {
    let (base, recorded) = mock_bot(vec![]);
    let notifier = notifier(&base, Some("900001"));
    let started = std::time::Instant::now();
    notifier.send(DEFAULT_TARGET, None, "第一条").await.unwrap();
    notifier.send(DEFAULT_TARGET, None, "第二条").await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "两条消息之间没有间隔：{:?}",
        started.elapsed()
    );
    assert_eq!(recorded.lock().unwrap().len(), 2);
}

/// 进程级共享：网关回复与投递推送是两条路，但发的是同一个会话——必须共用的闸门。
#[tokio::test]
async fn gate_is_shared_across_notifiers() {
    let (base, _) = mock_bot(vec![]);
    let gateway_side = notifier(&base, Some("900006"));
    let dispatcher_side = notifier(&base, Some("900006"));
    let started = std::time::Instant::now();
    gateway_side.send(DEFAULT_TARGET, None, "网关回复").await.unwrap();
    dispatcher_side.send(DEFAULT_TARGET, None, "投递推送").await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "两个 notifier 各发各的，会连发撞限速：{:?}",
        started.elapsed()
    );
}

/// 不同会话互不阻塞：一个会话排队不该拖住另一个。
#[tokio::test]
async fn different_chats_do_not_block_each_other() {
    let (base, _) = mock_bot(vec![]);
    let notifier = notifier(&base, Some("900002"));
    let started = std::time::Instant::now();
    notifier.send("chat:900002a", None, "a").await.unwrap();
    notifier.send("chat:900002b", None, "b").await.unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "不同会话不该互相等：{:?}",
        started.elapsed()
    );
}

/// 正文里的复选框展开成可点深链（点一下 Telegram 会把命令发回来）。
#[tokio::test]
async fn todo_checkbox_becomes_an_inline_deep_link() {
    let (base, recorded) = mock_bot(vec![
        // 第一次调用是 getMe（取 bot 用户名）。
        r#"{"ok":true,"result":{"id":1,"username":"ru41bot"}}"#,
        r#"{"ok":true,"result":{"message_id":21}}"#,
    ]);
    let notifier = notifier(&base, Some("900007"));
    let text = format!(
        "- {} 板子焊接",
        ru41::interactive::cmd_token("☐", "/完成 abc123")
    );
    notifier.send(DEFAULT_TARGET, None, &text).await.unwrap();
    let recorded = recorded.lock().unwrap();
    let body = &recorded[1].body;
    // body 是 JSON 原文，HTML 里的引号被转义成 \"，断言避开引号。
    assert!(body.contains("tg://resolve?domain=ru41bot"), "{body}");
    assert!(body.contains(">☐</a>"), "{body}");
    assert!(body.contains("板子焊接"), "{body}");
    assert!(!body.contains("{{cmd:"), "{body}");
}

/// 没有可点指令的普通消息：不额外去取 bot 用户名（少一次 API 调用）。
#[tokio::test]
async fn plain_message_does_not_fetch_bot_username() {
    let (base, recorded) = mock_bot(vec![r#"{"ok":true,"result":{"message_id":22}}"#]);
    let notifier = notifier(&base, Some("900008"));
    notifier.send(DEFAULT_TARGET, None, "普通消息").await.unwrap();
    let recorded = recorded.lock().unwrap();
    assert_eq!(recorded.len(), 1, "多发了一次请求：{:?}", recorded);
    assert!(recorded[0].body.contains("普通消息"));
}

/// 长文本被切成多条时，每条都要排进同一会话的发送队列。
#[tokio::test]
async fn long_message_chunks_are_spaced_out() {
    let (base, recorded) = mock_bot(vec![]);
    let notifier = notifier(&base, Some("900004"));
    let body = "正文段落。\n\n".repeat(900);
    let started = std::time::Instant::now();
    notifier.send(DEFAULT_TARGET, None, &body).await.unwrap();
    let count = recorded.lock().unwrap().len();
    assert!(count > 1, "长文本应被切片");
    assert!(
        started.elapsed() >= Duration::from_millis(1000),
        "切片之间没有间隔：{:?}",
        started.elapsed()
    );
}
