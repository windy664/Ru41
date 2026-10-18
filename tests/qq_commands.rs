use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::task::TaskService,
    channels::qq::commands::{self, Command, CommandContext, QqCommandHandler},
    domain::{NotifyRoute, TaskStatus},
    modules::{
        ecard::EcardQqCommand,
        papers::{ArxivClient, PapersQqCommand},
    },
    ports::storage::TaskStore,
};
use std::sync::Arc;

#[test]
fn parses_fixed_commands() {
    assert_eq!(commands::parse_command("帮助"), Some(Command::Help));
    assert_eq!(commands::parse_command(" /Help "), Some(Command::Help));
    assert_eq!(commands::parse_command("任务"), Some(Command::List));
    assert_eq!(commands::parse_command("tasks"), Some(Command::List));
    assert_eq!(commands::parse_command("任务 3"), Some(Command::Show(3)));
    assert_eq!(commands::parse_command("task 5"), Some(Command::Show(5)));
    assert_eq!(commands::parse_command("取消 2"), Some(Command::Cancel(2)));
    assert_eq!(
        commands::parse_command("cancel 7"),
        Some(Command::Cancel(7))
    );
    assert_eq!(
        commands::parse_command("绑定 abc123"),
        Some(Command::Bind("abc123".into()))
    );
    assert_eq!(
        commands::parse_command("bind xyz"),
        Some(Command::Bind("xyz".into()))
    );
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
    }
}

#[test]
fn list_scopes_to_current_conversation() {
    let fixture = fixture();
    let group_view =
        commands::execute(&Command::List, &ctx(&fixture, "group:G1", "U9", false)).unwrap();
    assert!(group_view.contains(&format!("#{}", fixture.group_task)));
    assert!(!group_view.contains(&format!("#{}", fixture.c2c_task)));
    let c2c_view =
        commands::execute(&Command::List, &ctx(&fixture, "user:U1", "U1", true)).unwrap();
    assert!(!c2c_view.contains(&format!("#{}", fixture.group_task)));
    assert!(c2c_view.contains(&format!("#{}", fixture.c2c_task)));
}

#[test]
fn bound_user_aggregates_across_conversations_in_c2c() {
    let fixture = fixture();
    let bound = commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(bound.contains("绑定成功"));
    let view = commands::execute(&Command::List, &ctx(&fixture, "user:U1", "U1", true)).unwrap();
    assert!(view.contains(&format!("#{}", fixture.group_task)));
    assert!(view.contains(&format!("#{}", fixture.c2c_task)));
    // 群聊中即使已绑定也不聚合，私人内容不进群。
    let group_view =
        commands::execute(&Command::List, &ctx(&fixture, "group:G1", "U1", false)).unwrap();
    assert!(!group_view.contains(&format!("#{}", fixture.c2c_task)));
}

async fn utility_reply(fixture: &Fixture, target: &str, author: &str, is_c2c: bool) -> String {
    let context = ctx(fixture, target, author, is_c2c);
    EcardQqCommand.handle("", &context).await.unwrap().unwrap()
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
    assert!(reply.contains("先绑定"));
    // 群聊（即使已绑定）：不展示私人数据。
    commands::execute(
        &Command::Bind("s3cret".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    let reply = utility_reply(&fixture, "group:G1", "U1", false).await;
    assert!(reply.contains("单聊"));
    // 绑定后单聊：返回余额与数据时间。
    let reply = utility_reply(&fixture, "user:U1", "U1", true).await;
    assert!(reply.contains("18.87度"));
    assert!(reply.contains("2.80吨"));
    assert!(reply.contains("测试楼A栋 A#101"));
    assert!(reply.contains("数据时间"));
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
    assert!(reply.contains("暂无水电读数"));
}

#[test]
fn extension_commands_match_aliases_and_join_help() {
    let ecard = EcardQqCommand;
    for verb in ["水电", "电费", "水费", "utility"] {
        assert!(ecard.matches(verb));
    }
    assert!(!ecard.matches("电费怎么交"));

    let papers = PapersQqCommand::new(ArxivClient::new(), String::new(), "codex".into());
    for verb in ["精读", "deep", "deepread"] {
        assert!(papers.matches(verb));
    }
    assert!(!papers.matches("精读一下"));

    let handlers: Vec<Arc<dyn QqCommandHandler>> = vec![Arc::new(papers), Arc::new(ecard)];
    let help = commands::help_text(&handlers);
    assert!(help.contains("精读 <arxiv-id>"));
    assert!(help.contains("水电 - 查询宿舍水电余额"));
    assert!(help.contains("其他消息将作为任务交给 Agent 处理"));
}

#[test]
fn bind_requires_configured_and_matching_secret() {
    let fixture = fixture();
    let disabled = CommandContext {
        bind_secret: None,
        ..ctx(&fixture, "user:U1", "U1", true)
    };
    let reply = commands::execute(&Command::Bind("s3cret".into()), &disabled).unwrap();
    assert!(reply.contains("未开放"));
    let wrong = commands::execute(
        &Command::Bind("nope".into()),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(wrong.contains("口令错误"));
}

#[test]
fn show_and_cancel_reject_cross_conversation_tasks() {
    let fixture = fixture();
    let denied = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
    )
    .unwrap();
    assert!(denied.contains("无权"));
    let denied = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "group:G1", "U9", false),
    )
    .unwrap();
    assert!(denied.contains("无权"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Queued
    );
    let cancelled = commands::execute(
        &Command::Cancel(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(cancelled.contains("已取消"));
    assert_eq!(
        fixture.store.get(fixture.c2c_task).unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
    let detail = commands::execute(
        &Command::Show(fixture.c2c_task),
        &ctx(&fixture, "user:U1", "U1", true),
    )
    .unwrap();
    assert!(detail.contains("已取消"));
    assert!(detail.contains("单聊的分析请求"));
}
