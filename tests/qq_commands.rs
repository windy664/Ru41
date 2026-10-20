use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::task::TaskService,
    channels::qq::commands::{self, Command},
    domain::{NotifyRoute, TaskStatus},
    modules::{
        ecard::EcardQqCommand,
        papers::{ArxivClient, PapersQqCommand},
        planner::TodoQqCommand,
    },
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

async fn utility_reply(fixture: &Fixture, target: &str, author: &str, is_c2c: bool) -> Reply {
    let context = ctx(fixture, target, author, is_c2c);
    let command = EcardQqCommand::new("codex".into());
    command.handle("", &context).await.unwrap().unwrap()
}

#[tokio::test]
async fn utility_command_requires_binding_and_c2c() {
    let fixture = fixture();
    // 写入一条读数。
    fixture
        .store
        .put_document(
            "ecard_reading",
            "2026-10-14",
            None,
            &serde_json::json!({"power": 18.87, "water": 2.8, "room": "测试楼A栋 A#101", "ts": 1_760_000_000_i64}),
        )
        .unwrap();
    // 未绑定：提示先绑定。
    let reply = utility_reply(&fixture, "user:U1", "U1", true).await;
    assert!(reply.text.contains("身份绑定"));
    // 群聊（即使已绑定）：不展示私人数据。
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    let reply = utility_reply(&fixture, "group:G1", "U1", false).await;
    assert!(reply.text.contains("单聊"));
    // 绑定后单聊：返回余额与数据时间。
    let reply = utility_reply(&fixture, "user:U1", "U1", true).await;
    assert!(reply.text.contains("18.87度"));
    assert!(reply.text.contains("2.80吨"));
    assert!(reply.text.contains("测试楼A栋 A#101"));
    assert!(reply.text.contains("数据时间"));
}

#[tokio::test]
async fn utility_command_without_reading() {
    let fixture = fixture();
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    let reply = utility_reply(&fixture, "user:U1", "U1", true).await;
    assert!(reply.text.contains("暂无水电读数"));
}

#[tokio::test]
async fn utility_natural_language_enqueues_authorized_analysis() {
    let fixture = fixture();
    for (date, power, water) in [("2026-10-13", 20.0, 3.0), ("2026-10-14", 18.5, 2.8)] {
        fixture
            .store
            .put_document(
                "ecard_reading",
                date,
                None,
                &serde_json::json!({"power": power, "water": water, "room": "测试楼A栋 A#101", "ts": 1_760_000_000_i64}),
            )
            .unwrap();
    }
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();

    let context = ctx(&fixture, "user:U1", "U1", true);
    let command = EcardQqCommand::new("codex".into());
    let reply = command
        .handle_message("帮我看看宿舍电费还剩多少，顺便分析一下", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(reply.text.contains("正在分析水电"));
    assert!(reply.text.contains("2026-10-14"));

    let task = fixture
        .store
        .list(10)
        .unwrap()
        .into_iter()
        .find(|task| task.request_key.ends_with(":ecard-analysis"))
        .unwrap();
    assert_eq!(task.backend_id, "codex");
    assert!(task.prompt.contains("帮我看看宿舍电费还剩多少"));
    assert!(task.prompt.contains("2026-10-13"));
    assert!(task.prompt.contains("2026-10-14"));
    assert!(task.prompt.contains("18.5"));
    assert!(task.prompt.contains("2.8"));
    assert!(!task.prompt.contains("测试楼A栋"));
    let notify = task.notify.unwrap();
    assert_eq!(notify.target, "user:U1");
    assert_eq!(notify.msg_id.as_deref(), Some("command-msg"));
}

#[tokio::test]
async fn utility_recent_phrase_enqueues_authorized_analysis() {
    let fixture = fixture();
    fixture
        .store
        .put_document(
            "ecard_reading",
            "2026-10-18",
            None,
            &serde_json::json!({"power": 26.77, "water": 0.8, "room": "测试楼A栋 A#101", "ts": 1_760_000_000_i64}),
        )
        .unwrap();
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();

    let context = ctx(&fixture, "user:U1", "U1", true);
    let command = EcardQqCommand::new("codex".into());
    let reply = command
        .handle_message("我宿舍近期水电费", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(reply.text.contains("正在分析水电"));
    let task = fixture
        .store
        .list(10)
        .unwrap()
        .into_iter()
        .find(|task| task.request_key.ends_with(":ecard-analysis"))
        .unwrap();
    assert!(task.prompt.contains("我宿舍近期水电费"));
    assert!(task.prompt.contains("2026-10-18"));
    assert!(task.prompt.contains("26.77"));
}

#[tokio::test]
async fn utility_natural_language_keeps_privacy_boundary() {
    let fixture = fixture();
    fixture
        .store
        .put_document(
            "ecard_reading",
            "2026-10-14",
            None,
            &serde_json::json!({"power": 18.5, "water": 2.8, "room": "测试楼A栋 A#101", "ts": 1_760_000_000_i64}),
        )
        .unwrap();
    let command = EcardQqCommand::new("codex".into());

    let unrelated = command
        .handle_message(
            "帮我总结一下今天的收件箱",
            &ctx(&fixture, "user:U1", "U1", true),
        )
        .await
        .unwrap();
    assert!(unrelated.is_none());
    let unbound = command
        .handle_message("电费还剩多少", &ctx(&fixture, "user:U1", "U1", true))
        .await
        .unwrap()
        .unwrap();
    assert!(unbound.text.contains("身份绑定"));
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    let group = command
        .handle_message("电费还剩多少", &ctx(&fixture, "group:G1", "U1", false))
        .await
        .unwrap()
        .unwrap();
    assert!(group.text.contains("单聊"));
    assert_eq!(fixture.store.list(10).unwrap().len(), 2);
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
    let ecard = EcardQqCommand::new("codex".into());
    for verb in ["水电", "电费", "水费", "utility"] {
        assert!(ecard.matches(verb));
    }
    assert!(!ecard.matches("电费怎么交"));

    let papers = PapersQqCommand::new(ArxivClient::new(), String::new(), "codex".into());
    for verb in ["精读", "deep", "deepread"] {
        assert!(papers.matches(verb));
    }
    assert!(!papers.matches("精读一下"));

    let todo = TodoQqCommand::new(ru41::domain::todo::TodoOptions::default());
    for verb in ["待办", "记待办", "完成", "推迟", "取消待办", "恢复待办"] {
        assert!(todo.matches(verb));
    }

    let handlers: Vec<Arc<dyn CommandHandler>> =
        vec![Arc::new(papers), Arc::new(ecard), Arc::new(todo)];
    let help = commands::help_text(&handlers);
    assert!(help.contains("## Ru41 机器人"));
    assert!(help.contains("**扩展指令**"));
    assert!(help.contains("精读 <arxiv-id>"));
    assert!(help.contains("`/水电`：查询宿舍水电余额"));
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
