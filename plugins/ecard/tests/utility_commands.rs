//! 水电 QQ 命令的行为测试（原 tests/qq_commands.rs 的 utility_*，随模块迁入）。

use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::task::TaskService,
    channels::qq::commands::{self, Command},
    domain::NotifyRoute,
    ports::commands::{CommandContext, CommandHandler, Reply},
    ports::storage::TaskStore,
};
use std::sync::Arc;

struct Fixture {
    store: Arc<dyn TaskStore>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let service = TaskService::new(store.clone());
    // 隐私边界测试需要两条存量任务（群/单聊各一）做计数锚点。
    service
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
        .unwrap();
    service
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
        .unwrap();
    Fixture { store, _dir: dir }
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

use ru41_plugin_ecard::EcardQqCommand;

#[test]
fn utility_matches_aliases() {
    let ecard = EcardQqCommand::new("codex".into());
    for verb in ["水电", "电费", "水费", "utility"] {
        assert!(ecard.matches(verb));
    }
    assert!(!ecard.matches("电费怎么交"));
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
