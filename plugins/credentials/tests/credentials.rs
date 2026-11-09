use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    channels::qq::commands::{self, Command},
    ports::commands::{CommandContext, CommandHandler},
    ports::storage::TaskStore,
};
use ru41_plugin_credentials::CredentialQqCommand;
use std::sync::Arc;

struct Fixture {
    store: Arc<dyn TaskStore>,
    _dir: tempfile::TempDir,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
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
        msg_id: "m1",
        author_openid: author,
        is_c2c,
        reply_channel: "qq",
        image_url: None,
        attachments: &[],
    }
}

fn bind(fixture: &Fixture, ctx: &CommandContext<'_>) {
    commands::execute(
        &Command::Bind("s3cret".into()),
        ctx,
        &ru41::app::usage_report::Pricing::builtin(),
    )
    .unwrap();
}

#[tokio::test]
async fn store_intent_consumed_before_agent_and_never_echoes_value() {
    let fixture = fixture();
    let context = ctx(&fixture, "user:U1", "U1", true);
    let command = CredentialQqCommand;
    bind(&fixture, &context);

    let reply = command
        .handle_message("帮我记住 ftp 的密码是 upload_80894", &context)
        .await
        .unwrap()
        .unwrap();
    // 回执给唯一标识 + 标签，不回显真值。
    assert!(reply.text.contains("已存凭据 `cred_"), "{}", reply.text);
    assert!(reply.text.contains("标签：ftp"), "{}", reply.text);
    assert!(!reply.text.contains("upload_80894"));

    // 入库且可通过端口读回（核心 CLI secret 读同一份记录）。
    let docs = fixture.store.documents("credential", 10).unwrap();
    assert_eq!(docs.len(), 1);
    let doc = &docs[0];
    assert!(doc.id.starts_with("cred_"), "{}", doc.id);
    assert_eq!(doc.value["value"].as_str().unwrap(), "upload_80894");
    assert_eq!(doc.value["label"].as_str().unwrap(), "ftp");

    // 无意图词的陈述不拦截，继续走 agent 通道。
    assert!(
        command
            .handle_message("ftp 的密码是 upload_80894", &context)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn group_chat_and_unbound_are_rejected() {
    let fixture = fixture();
    let command = CredentialQqCommand;
    // 群聊：即使命中意图也拒绝（凭据不泄露给群）。
    let group = ctx(&fixture, "group:G1", "U1", false);
    bind(&fixture, &group);
    let reply = command
        .handle_message("帮我记住 ftp 的密码是 upload_80894", &group)
        .await
        .unwrap()
        .unwrap();
    assert!(reply.text.contains("单聊"));
    assert!(fixture.store.documents("credential", 10).unwrap().is_empty());

    // 未绑定：拒绝。
    let c2c = ctx(&fixture, "user:U2", "U2", true);
    let reply = command
        .handle_message("帮我记住 ftp 的密码是 upload_80894", &c2c)
        .await
        .unwrap()
        .unwrap();
    assert!(reply.text.contains("身份绑定"));
    assert!(fixture.store.documents("credential", 10).unwrap().is_empty());
}
