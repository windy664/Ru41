use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::task::TaskService,
    channels::qq::commands::{self, Command},
    domain::{NotifyRoute, TaskStatus},
    modules::planner::TodoQqCommand,
    ports::commands::{CommandContext, CommandHandler, Reply},
    ports::storage::TaskStore,
};
use std::sync::Arc;

#[test]
fn parses_fixed_commands() {
    for input in ["帮助", "/帮助", "help", "/help", " /Help "] {
        assert_eq!(commands::parse_command(input), Some(Command::Help));
    }
    for input in ["任务", "/任务", "tasks", "/tasks"] {
        assert_eq!(commands::parse_command(input), Some(Command::List));
    }
    for input in ["任务 3", "/任务 3", "task 3", "/task 3", "show 3"] {
        assert_eq!(commands::parse_command(input), Some(Command::Show(3)));
    }
    for input in ["取消 2", "/取消 2", "cancel 2", "/cancel 2"] {
        assert_eq!(commands::parse_command(input), Some(Command::Cancel(2)));
    }
    for input in ["绑定 abc123", "/绑定 abc123", "bind abc123", "/bind abc123"] {
        assert_eq!(
            commands::parse_command(input),
            Some(Command::Bind("abc123".into()))
        );
    }
    for input in ["新建", "/新建", "clear", "/clear"] {
        assert_eq!(commands::parse_command(input), Some(Command::Clear));
    }
}

#[test]
fn natural_language_falls_through_to_agent() {
    assert_eq!(commands::parse_command("任务怎么做"), None);
    assert_eq!(commands::parse_command("任务 abc"), None);
    assert_eq!(commands::parse_command("取消"), None);
    assert_eq!(commands::parse_command("绑定"), None);
    assert_eq!(commands::parse_command("帮我总结一下今天的收件箱"), None);
}

struct Fixture {
    store: Arc<dyn TaskStore>,
    group_task: i64,
    c2c_task: i64,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let service = TaskService::new(store.clone());
    let group_task = service
        .enqueue_with_notify(
            "qq:msg:g1",
            "群里的分析请求",
            "codex",
            &NotifyRoute {
                channel: "qq".into(),
                target: "group:G1".into(),
                msg_id: Some("m1".into()),
            },
        )
        .unwrap()
        .task
        .id;
    let c2c_task = service
        .enqueue_with_notify(
            "qq:msg:u1",
            "单聊的分析请求",
            "codex",
            &NotifyRoute {
                channel: "qq".into(),
                target: "user:U1".into(),
                msg_id: Some("m2".into()),
            },
        )
        .unwrap()
        .task
        .id;
    Fixture {
        store,
        group_task,
        c2c_task,
        _dir: dir,
    }
}

fn ctx<'a>(
    fixture: &'a Fixture,
    target: &'a str,
    author: &'a str,
    is_c2c: bool,
) -> CommandContext<'a> {
    CommandContext {
        store: &fixture.store,
        bind_secret: Some("s3cret"),
        target,
        msg_id: "command-msg",
        author_openid: author,
        is_c2c,
        reply_channel: "qq",
        image_url: None,
    }
}

#[test]
fn list_scopes_to_current_conversation() {
    let fixture = fixture();
    let group_view =
        commands::execute(&Command::List, &ctx(&fixture, "group:G1", "U9", false)).unwrap();
    assert!(
        group_view
            .text
            .contains(&format!("#{}", fixture.group_task))
    );
    assert!(!group_view.text.contains(&format!("#{}", fixture.c2c_task)));
    // 任务列表带「详情/取消」按钮行，指令指向对应任务。
    let markup = group_view.markup.expect("task list must carry buttons");
    let first = &markup.rows[0];
    assert!(
        first[0]
            .command
            .contains(&format!("/任务 {}", fixture.group_task))
    );
    assert!(
        first[1]
            .command
            .contains(&format!("/取消 {}", fixture.group_task))
    );
    let c2c_view =
        commands::execute(&Command::List, &ctx(&fixture, "user:U1", "U1", true)).unwrap();
    assert!(!c2c_view.text.contains(&format!("#{}", fixture.group_task)));
    assert!(c2c_view.text.contains(&format!("#{}", fixture.c2c_task)));
}

#[test]
fn clear_resets_only_the_current_conversation() {
    let fixture = fixture();
    fixture
        .store
        .put_document(
            "conversation",
            "qq:user:U1",
            None,
            &serde_json::json!({"backend_id": "codex", "session_id": "thread-1"}),
        )
        .unwrap();
    let reply = commands::execute(&Command::Clear, &ctx(&fixture, "user:U1", "U1", true)).unwrap();
    assert!(reply.text.contains("新对话"));
    let document = fixture
        .store
        .document("conversation", "qq:user:U1")
        .unwrap()
        .unwrap();
    assert!(document.value["session_id"].is_null());
}

#[test]
fn bound_user_aggregates_across_conversations_in_c2c() {
    let fixture = fixture();
    let bound = commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(bound.text.contains("绑定成功"));
    let view = commands::execute(&Command::List, &ctx(&fixture, "user:U1", "U1", true)).unwrap();
    assert!(view.text.contains(&format!("#{}", fixture.group_task)));
    assert!(view.text.contains(&format!("#{}", fixture.c2c_task)));
    // 群聊中即使已绑定也不聚合，私人内容不进群。
    let group_view =
        commands::execute(&Command::List, &ctx(&fixture, "group:G1", "U1", false)).unwrap();
    assert!(!group_view.text.contains(&format!("#{}", fixture.c2c_task)));
}

#[tokio::test]
async fn todo_commands_add_list_and_complete_in_private_chat() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default());
    let context = ctx(&fixture, "user:U1", "U1", true);

    let unbound = command
        .handle_command("待办", "", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(unbound.text.contains("身份绑定"));
    commands::execute(&Command::Bind("s3cret".into()), &context).unwrap();

    let added = command
        .handle_command("记待办", "明天下午3点 写项目周报", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(added.text.contains("已添加待办"));
    assert!(added.text.contains("写项目周报"));

    let documents = fixture.store.documents("todo_item", 10).unwrap();
    assert_eq!(documents.len(), 1);
    let item: ru41::domain::todo::TodoItem =
        serde_json::from_value(documents[0].value.clone()).unwrap();
    assert_eq!(item.title, "写项目周报");
    assert!(item.due_at.is_some());

    let summary = command
        .handle_command("待办", "", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(summary.text.contains("待办摘要"));
    assert!(summary.text.contains("写项目周报"));

    let completed = command
        .handle_command("完成", item.short_id(), &context)
        .await
        .unwrap()
        .unwrap();
    assert!(completed.text.contains("已完成待办"));
    assert!(completed.text.contains("写项目周报"));

    let empty = command
        .handle_command("待办", "", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(empty.text.contains("暂无未完成待办"));
}

#[tokio::test]
async fn todo_commands_reject_group_chat_and_parse_views() {
    let fixture = fixture();
    let command = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default());
    let private = ctx(&fixture, "user:U1", "U1", true);
    commands::execute(&Command::Bind("s3cret".into()), &private).unwrap();
    command
        .handle_command("记待办", "未排期事项", &private)
        .await
        .unwrap();

    let group = command
        .handle_command("待办", "", &ctx(&fixture, "group:G1", "U1", false))
        .await
        .unwrap()
        .unwrap();
    assert!(group.text.contains("群聊"));

    let inbox = command
        .handle_command("待办", "收件箱", &private)
        .await
        .unwrap()
        .unwrap();
    assert!(inbox.text.contains("收件箱"));
    assert!(inbox.text.contains("未排期事项"));

    let overdue = command
        .handle_command("待办", "逾期", &private)
        .await
        .unwrap()
        .unwrap();
    assert!(overdue.text.contains("暂无"));
}

#[test]
fn extension_commands_match_aliases_and_join_help() {
    let todo = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default());
    for verb in ["待办", "记待办", "完成", "推迟", "取消待办", "恢复待办"] {
        assert!(todo.matches(verb));
    }

    let handlers: Vec<Arc<dyn CommandHandler>> = vec![Arc::new(todo)];
    let help = commands::help_text(&handlers);
    assert!(help.contains("## Ru41 机器人"));
    assert!(help.contains("**扩展指令**"));
    assert!(help.contains("`/待办`：今日、逾期、长期事项"));
    assert!(help.contains("当前对话会自动保留上下文"));
}

#[test]
fn bind_requires_configured_and_matching_secret() {
    let fixture = fixture();
    let disabled = CommandContext {
        bind_secret: None,
        ..ctx(&fixture, "user:U1", "U1", true)
    };
    let reply = commands::execute(&Command::Bind("s3cret".into()), &disabled).unwrap();
    assert!(reply.text.contains("未开放"));
    let wrong = commands::execute(
        &Command::Bind("nope".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(wrong.text.contains("口令错误"));
}

#[test]
fn show_and_cancel_reject_cross_conversation_tasks() {
    let fixture = fixture();
    let denied = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
    )
    .unwrap();
    assert!(denied.text.contains("无权"));
    let denied = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
    )
    .unwrap();
    assert!(denied.text.contains("无权"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    let cancelled = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(cancelled.text.contains("已取消"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    let detail = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(detail.text.contains("已取消"));
    assert!(detail.text.contains("单聊的分析请求"));
}

#[test]
fn c2c_new_message_interrupts_unfinished_tasks_same_conversation() {
    use ru41::channels::qq::QqBot;
    use ru41::channels::qq::api::QqApiClient;
    use ru41::channels::qq::gateway::GatewayEvent;

    let fixture = fixture();
    let bot = QqBot {
        api: Arc::new(QqApiClient::new("app", "secret")),
        store: fixture.store.clone(),
        backend: "codex".into(),
        bind_secret: None,
        command_handlers: vec![],
        image_consumers: vec![],
        sequencer: Arc::new(ru41::channels::qq::ReplySequencer::default()),
    };
    let c2c = |msg_id: &str, content: &str| GatewayEvent::C2cMessage {
        msg_id: msg_id.into(),
        content: content.into(),
        user_openid: "U777".into(),
        image_url: None,
        attachments: vec![],
    };

    // 第一条直接入队，不发回执（用户直接看活动流）。
    let receipt = bot.ingest(&c2c("m1", "第一条")).unwrap();
    assert!(receipt.created);
    assert_eq!(receipt.interrupted, 0);
    assert!(receipt.interrupt_text().is_none());
    // 同会话第二条：打断第一条（排队中），只回打断提示。
    let receipt = bot.ingest(&c2c("m2", "第二条")).unwrap();
    assert_eq!(receipt.interrupted, 1);
    assert_eq!(
        receipt.interrupt_text().as_deref(),
        Some("⏹ 已打断上一条未完成的任务（1 个）")
    );
    // 重复消息（同 msg_id）：不重复创建任务，也不回执。
    let receipt = bot.ingest(&c2c("m2", "第二条")).unwrap();
    assert!(!receipt.created);
    assert!(receipt.interrupt_text().is_none());
    // 群消息不打断单聊任务，也不回执。
    let group = GatewayEvent::GroupMessage {
        msg_id: "g9".into(),
        content: "群消息".into(),
        group_openid: "G1".into(),
        author_openid: "U9".into(),
        attachments: vec![],
    };
    let receipt = bot.ingest(&group).unwrap();
    assert_eq!(receipt.interrupted, 0);
    assert!(receipt.interrupt_text().is_none());

    let tasks = fixture.store.list(10).unwrap();
    let status_of = |key: &str| {
        tasks
            .iter()
            .find(|t| t.request_key == key)
            .map(|t| t.status)
    };
    assert_eq!(status_of("qq:msg:m1"), Some(TaskStatus::Cancelled));
    assert_eq!(status_of("qq:msg:m2"), Some(TaskStatus::Queued));
    assert_eq!(status_of("qq:msg:g9"), Some(TaskStatus::Queued));
}
