use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    channels::telegram::{TelegramNotifier, api::TelegramApi, bot::TelegramBot},
    ports::storage::TaskStore,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

const TOKEN: &str = "123456:test-token-value";

/// mock Bot API：按方法名路由——getUpdates 吐队列里的更新，其余记录后回 ok。
struct MockBot {
    base: String,
    updates: Arc<Mutex<Vec<serde_json::Value>>>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl MockBot {
    fn new(updates: Vec<serde_json::Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let queue = Arc::new(Mutex::new(updates));
        let sent = Arc::new(Mutex::new(Vec::new()));
        let queue_clone = Arc::clone(&queue);
        let sent_clone = Arc::clone(&sent);
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
                let body = String::from_utf8_lossy(&body).into_owned();
                let payload = if path.ends_with("/getUpdates") {
                    let mut queue = queue_clone.lock().unwrap();
                    let updates = if queue.is_empty() {
                        Vec::new()
                    } else {
                        vec![queue.remove(0)]
                    };
                    serde_json::json!({"ok": true, "result": updates}).to_string()
                } else {
                    sent_clone.lock().unwrap().push(body);
                    serde_json::json!({"ok": true, "result": {"message_id": 1}}).to_string()
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
        Self {
            base,
            updates: queue,
            sent,
        }
    }

    fn sent(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    fn pending_updates(&self) -> usize {
        self.updates.lock().unwrap().len()
    }
}

fn message_update(
    update_id: i64,
    message_id: i64,
    chat_id: i64,
    user_id: i64,
    text: &str,
) -> serde_json::Value {
    serde_json::json!({
        "update_id": update_id,
        "message": {
            "message_id": message_id,
            "chat": {"id": chat_id, "type": "private"},
            "from": {"id": user_id},
            "text": text,
        }
    })
}

fn bot(mock: &MockBot, store: Arc<dyn TaskStore>, allowed: Vec<i64>) -> Arc<TelegramBot> {
    let api = Arc::new(TelegramApi::with_base(TOKEN, mock.base.clone()));
    Arc::new(TelegramBot {
        notifier: TelegramNotifier::new(api.clone(), Some("999".into())),
        api,
        store,
        backend: "codex".into(),
        bind_secret: None,
        command_handlers: Vec::new(),
        allowed_user_ids: allowed,
        pricing: ru41::app::usage_report::Pricing::builtin(),
    })
}

fn store() -> (tempfile::TempDir, Arc<dyn TaskStore>) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    (dir, store)
}

#[tokio::test]
async fn natural_language_is_enqueued_with_telegram_route() {
    let mock = MockBot::new(vec![message_update(1, 7, 12345, 678, "帮我总结今天的课表")]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    bot.handle_update(&message_update(1, 7, 12345, 678, "帮我总结今天的课表"))
        .await;

    let tasks = store.list(50).unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(task.request_key, "telegram:msg:7");
    let notify = task.notify.as_ref().unwrap();
    assert_eq!(notify.channel, "telegram");
    assert_eq!(notify.target, "chat:12345");
    assert_eq!(notify.msg_id.as_deref(), Some("7"));
}

#[tokio::test]
async fn duplicate_message_does_not_enqueue_twice() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    let update = message_update(1, 7, 12345, 678, "重复消息");
    bot.handle_update(&update).await;
    bot.handle_update(&update).await;

    assert_eq!(store.list(50).unwrap().len(), 1);
}

#[tokio::test]
async fn builtin_command_is_answered_in_chat() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    bot.handle_update(&message_update(1, 7, 12345, 678, "/tasks"))
        .await;

    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains(r#""chat_id":"12345""#));
    assert!(sent[0].contains("任务列表"));
}

#[tokio::test]
async fn non_whitelisted_user_is_ignored() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), vec![111]);

    bot.handle_update(&message_update(1, 7, 12345, 222, "我是外人"))
        .await;

    assert!(store.list(50).unwrap().is_empty());
    assert!(mock.sent().is_empty());
}

#[tokio::test]
async fn polling_loop_consumes_updates_and_persists_offset() {
    let mock = MockBot::new(vec![message_update(5, 9, 12345, 678, "轮询入队")]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    let (tx, rx) = watch::channel(false);
    let runner = tokio::spawn({
        let bot = bot.clone();
        async move { bot.run(rx).await }
    });

    // 等网关把这条消息消费掉并落库。
    let mut task = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let tasks = store.list(50).unwrap();
        if !tasks.is_empty() {
            task = Some(tasks[0].clone());
            break;
        }
    }
    let task = task.expect("message was not ingested");
    assert_eq!(task.request_key, "telegram:msg:9");

    tx.send(true).unwrap();
    let _ = runner.await;

    // offset 落库：重启后从上次继续，不重复消费。
    let offset = store
        .document("telegram", "offset")
        .unwrap()
        .expect("offset document missing");
    assert_eq!(offset.value["offset"].as_i64(), Some(6));
    assert_eq!(mock.pending_updates(), 0);
}
