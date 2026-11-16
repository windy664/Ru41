use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    channels::telegram::{TelegramNotifier, api::TelegramApi, bot::TelegramBot},
    ports::channel::NotifyChannel,
    ports::commands::{MarkupButton, ReplyMarkup},
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

fn callback_update(
    update_id: i64,
    callback_id: &str,
    chat_id: i64,
    chat_type: &str,
    user_id: i64,
    message_id: i64,
    data: &str,
) -> serde_json::Value {
    serde_json::json!({
        "update_id": update_id,
        "callback_query": {
            "id": callback_id,
            "from": {"id": user_id},
            "message": {
                "message_id": message_id,
                "chat": {"id": chat_id, "type": chat_type},
            },
            "data": data,
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
    assert_eq!(task.request_key, "telegram:msg:12345:7");
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
    assert_eq!(task.request_key, "telegram:msg:12345:9");

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

/// 按钮点击：先应答（否则 Telegram 转圈），再把按钮里的指令文本当作用户消息执行。
#[tokio::test]
async fn callback_button_answers_query_and_runs_its_command() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    // 先造一条属于私聊 chat:12345 的任务，让任务列表有内容（列表才带按钮）。
    bot.handle_update(&message_update(1, 7, 12345, 678, "整理一下今天的课表"))
        .await;
    let task_id = store.list(50).unwrap()[0].id;

    bot.handle_update(&callback_update(
        2,
        "cb-1",
        12345,
        "private",
        678,
        42,
        "/tasks",
    ))
    .await;

    let sent = mock.sent();
    // 1) 必须先应答按钮。
    assert!(
        sent.iter().any(|body| body.contains("callback_query_id")),
        "按钮未被应答：{sent:?}"
    );
    // 2) 按钮里的指令被执行，结果回到原会话。
    let reply = sent
        .iter()
        .find(|body| body.contains(r#""chat_id":"12345""#))
        .expect("按钮指令没有产生回复");
    assert!(reply.contains("最近任务"), "{reply}");
    assert!(reply.contains(&format!("#{task_id}")), "{reply}");
    // 3) 回复自带 inline keyboard：点一下即完成，不需要再手打命令。
    assert!(reply.contains("inline_keyboard"), "{reply}");
    assert!(reply.contains("callback_data"), "{reply}");
}

/// planner 的待办回执出到 Telegram：正文是纯符号，动作落在底部按钮上（点一下即完成）。
#[tokio::test]
async fn todo_reply_lands_as_symbol_plus_inline_buttons() {
    let mock = MockBot::new(vec![]);
    let api = Arc::new(TelegramApi::with_base(TOKEN, mock.base.clone()));
    let notifier = TelegramNotifier::new(api, Some("999".into()));

    let mut markup = ReplyMarkup::new();
    markup.push_row(vec![MarkupButton::new("☐ 板子焊接", "/完成 abc123")]);
    let text = "## 待办（1）\n\n- {{cmd:☐|/完成 abc123}} 板子焊接 — 明天\n";

    NotifyChannel::send_with_markup(&notifier, "chat:12345", None, text, &markup)
        .await
        .unwrap();

    let body = &mock.sent()[0];
    // 正文：占位符降级成纯符号，命令不外泄。
    assert!(body.contains("☐ 板子焊接"), "{body}");
    assert!(!body.contains("{{cmd:"), "{body}");
    // 动作：底部 inline keyboard，按钮带渠道无关的命令文本。
    assert!(body.contains("inline_keyboard"), "{body}");
    assert!(body.contains("callback_data"), "{body}");
    assert!(body.contains("/完成 abc123"), "{body}");
}

/// message_id 只在会话内唯一：两个会话的同号消息不能互相顶掉
/// （幂等键必须带 chat，否则后一条被当成重复而丢弃）。
#[tokio::test]
async fn same_message_id_in_different_chats_does_not_collide() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    bot.handle_update(&message_update(1, 7, 12345, 678, "私聊的一条")).await;
    bot.handle_update(&message_update(2, 7, 999, 678, "群里的另一条")).await;

    let tasks = store.list(50).unwrap();
    assert_eq!(tasks.len(), 2, "两个会话的同号消息互相吞了");
}

/// 群里的按钮同样属于群聊：私人数据闸门不能被 callback 绕过。
#[tokio::test]
async fn callback_in_group_chat_cannot_reach_private_task() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), Vec::new());

    bot.handle_update(&message_update(1, 7, 12345, 678, "只属于我私聊的任务"))
        .await;
    let task_id = store.list(50).unwrap()[0].id;

    // 私聊里点按钮：看得见自己的任务。
    bot.handle_update(&callback_update(
        2,
        "cb-private",
        12345,
        "private",
        678,
        42,
        "/tasks",
    ))
    .await;
    let private_reply = mock
        .sent()
        .into_iter()
        .find(|body| body.contains(r#""chat_id":"12345""#))
        .expect("私聊按钮没有回复");
    assert!(private_reply.contains(&format!("#{task_id}")), "{private_reply}");

    // 群里点按钮：同一条任务不可见。
    bot.handle_update(&callback_update(
        3,
        "cb-group",
        999,
        "group",
        678,
        43,
        "/tasks",
    ))
    .await;
    let group_reply = mock
        .sent()
        .into_iter()
        .find(|body| body.contains(r#""chat_id":"999""#))
        .expect("群聊按钮没有回复");
    assert!(
        !group_reply.contains(&format!("#{task_id}")),
        "群聊按钮不得越过私人数据闸门：{group_reply}"
    );
}

/// 空白名单之外的点击者不得触发任何动作。
#[tokio::test]
async fn callback_from_non_whitelisted_user_is_ignored() {
    let mock = MockBot::new(vec![]);
    let (_dir, store) = store();
    let bot = bot(&mock, store.clone(), vec![111]);

    bot.handle_update(&callback_update(
        1,
        "cb-out",
        12345,
        "private",
        222,
        42,
        "/tasks",
    ))
    .await;

    // 应答照发（免得对方按钮一直转圈），但指令不执行。
    assert!(
        mock.sent()
            .iter()
            .all(|body| !body.contains(r#""chat_id""#)),
        "非白名单用户触发了执行：{:?}",
        mock.sent()
    );
    assert!(store.list(50).unwrap().is_empty());
}
