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

fn bind(_fixture: &Fixture, ctx: &CommandContext<'_>) {
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

    // 无意图词的陈述不拦截，继续走 agent 通道（标签相同也不算查询）。
    assert!(
        command
            .handle_message("ftp 的密码是 upload_80894", &context)
            .await
            .unwrap()
            .is_none()
    );

    // 查询：带标识 → 网关直接回明文（不进模型）。
    let by_id = command
        .handle_message(&format!("查一下 {}", doc.id), &context)
        .await
        .unwrap()
        .unwrap();
    assert!(by_id.text.contains("upload_80894"), "{}", by_id.text);
    assert!(by_id.text.contains("ftp"), "{}", by_id.text);
    // 查询：标签 + 查询意图 → 同样直接回。
    let by_label = command
        .handle_message("ftp 这个凭据是什么", &context)
        .await
        .unwrap()
        .unwrap();
    assert!(by_label.text.contains("upload_80894"), "{}", by_label.text);
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
    assert!(
        fixture
            .store
            .documents("credential", 10)
            .unwrap()
            .is_empty()
    );

    // 未绑定：拒绝。
    let c2c = ctx(&fixture, "user:U2", "U2", true);
    let reply = command
        .handle_message("帮我记住 ftp 的密码是 upload_80894", &c2c)
        .await
        .unwrap()
        .unwrap();
    assert!(reply.text.contains("身份绑定"));
    assert!(
        fixture
            .store
            .documents("credential", 10)
            .unwrap()
            .is_empty()
    );
}

/// 2026-11-24: 凭据托管 v1 的寻址键就是用户说的那个词(「ftp上传密码」)，
/// v2 改成唯一标识后那个入口消失了 —— 用户问「ftp 的凭据」只能拿到标识。
/// 别名把入口补回来: 别名命中时直接回明文。
#[tokio::test]
async fn alias_lookup_returns_plaintext() {
    let fixture = fixture();
    let command = CredentialQqCommand;
    let c2c = ctx(&fixture, "user:U1", "U1", true);
    bind(&fixture, &c2c);

    let stored = command
        .handle_message("帮我记住 ftp 的密码是 upload_80894", &c2c)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.text.contains("已存凭据"), "{}", stored.text);

    // 给它加别名「ftp」
    let docs = fixture.store.documents("credential", 10).unwrap();
    let doc = docs.first().expect("有一条凭据");
    let id = doc.id.clone();
    let revision = fixture
        .store
        .document("credential", &id)
        .unwrap()
        .map(|d| d.revision);
    let mut body = doc.value.clone();
    body["aliases"] = serde_json::json!(["ftp", "上传"]);
    fixture
        .store
        .put_document("credential", &id, revision, &body)
        .unwrap();

    // 问「ftp 的凭据是什么」→ 命中别名 → 回明文
    let reply = command
        .handle_message("ftp 的凭据是什么", &c2c)
        .await
        .unwrap()
        .unwrap();
    assert!(
        reply.text.contains("upload_80894"),
        "别名命中该回明文: {}",
        reply.text
    );
}

/// 一条别名能命中多条凭据(上传/下载都叫 ftp) → **一起回**，只给一条等于没答。
#[tokio::test]
async fn alias_lookup_returns_all_matches() {
    let fixture = fixture();
    let command = CredentialQqCommand;
    let c2c = ctx(&fixture, "user:U1", "U1", true);
    bind(&fixture, &c2c);

    for (alias, value) in [("ftp-上传", "upload_pw"), ("ftp-下载", "download_pw")] {
        let stored = command
            .handle_message(&format!("帮我记住 {alias} 的密码是 {value}"), &c2c)
            .await
            .unwrap()
            .unwrap();
        assert!(stored.text.contains("已存凭据"), "{}", stored.text);
    }
    // 两条都加别名 "ftp"
    for doc in fixture.store.documents("credential", 10).unwrap() {
        let id = doc.id.clone();
        let revision = fixture
            .store
            .document("credential", &id)
            .unwrap()
            .map(|d| d.revision);
        let mut body = doc.value.clone();
        body["aliases"] = serde_json::json!(["ftp"]);
        fixture
            .store
            .put_document("credential", &id, revision, &body)
            .unwrap();
    }

    let reply = command
        .handle_message("ftp 的凭据是什么", &c2c)
        .await
        .unwrap()
        .unwrap();
    assert!(
        reply.text.contains("upload_pw"),
        "应回第一条: {}",
        reply.text
    );
    assert!(
        reply.text.contains("download_pw"),
        "应回第二条: {}",
        reply.text
    );
}
