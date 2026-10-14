use ru41::{
    adapters::database::sqlite::SqliteTaskStore, app::interactions::InteractionService,
    domain::records::Interaction, ports::storage::TaskStore,
};
use std::sync::Arc;

fn service() -> (Arc<SqliteTaskStore>, InteractionService) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    // TempDir 在函数结束时销毁；借用其生命周期到测试结束。
    std::mem::forget(dir);
    let svc = InteractionService {
        store: store.clone(),
    };
    (store, svc)
}

fn interaction(expires_in: i64) -> Interaction {
    Interaction {
        id: "it-1".into(),
        run_id: "run-1-1".into(),
        request_id: serde_json::json!(7),
        method: "item/commandExecution/requestApproval".into(),
        detail: serde_json::json!({"command": "rm -rf /"}),
        options: vec![
            serde_json::json!({"decision": "accept"}),
            serde_json::json!({"decision": "decline"}),
        ],
        expires_at: ru41::runtime::worker::now() + expires_in,
        status: "pending".into(),
        response: None,
    }
}

#[tokio::test]
async fn reply_roundtrip_answers_pending_request() {
    let (store, svc) = service();
    // 运行与任务处于活跃状态，回复应当被接受。
    let task = store.enqueue("event", "hello").unwrap().task;
    let _claim = store
        .claim("worker", ru41::runtime::worker::now(), 60)
        .unwrap()
        .unwrap();
    store
        .put_document(
            "run",
            "run-1-1",
            None,
            &serde_json::json!({"id":"run-1-1","task_id":task.id,"attempt_id":_claim.attempt_id,"status":"running"}),
        )
        .unwrap();

    let requester = {
        let svc = svc.clone();
        tokio::spawn(async move { svc.request(interaction(60)).await })
    };
    std::thread::sleep(std::time::Duration::from_millis(50));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    svc.reply("it-1", serde_json::json!({"decision": "accept"}))
        .unwrap();
    assert_eq!(
        requester.await.unwrap().unwrap(),
        serde_json::json!({"decision": "accept"})
    );
    // 已回答的交互不能再被回答。
    assert!(
        svc.reply("it-1", serde_json::json!({"decision": "decline"}))
            .is_err()
    );
}

#[tokio::test]
async fn reply_rejects_unoffered_option_and_unknown_request() {
    let (store, svc) = service();
    let task = store.enqueue("event", "hello").unwrap().task;
    let _claim = store
        .claim("worker", ru41::runtime::worker::now(), 60)
        .unwrap()
        .unwrap();
    store
        .put_document(
            "run",
            "run-1-1",
            None,
            &serde_json::json!({"id":"run-1-1","task_id":task.id,"attempt_id":_claim.attempt_id,"status":"running"}),
        )
        .unwrap();
    store
        .put_document(
            "interaction",
            "it-1",
            None,
            &serde_json::to_value(interaction(60)).unwrap(),
        )
        .unwrap();
    // 不在选项中的决定被拒绝。
    assert!(
        svc.reply("it-1", serde_json::json!({"decision": "acceptForSession"}))
            .is_err()
    );
    // 未知交互被拒绝。
    assert!(
        svc.reply("missing", serde_json::json!({"decision": "accept"}))
            .is_err()
    );
}

#[tokio::test]
async fn expired_request_fails_instead_of_blocking_forever() {
    let (_, svc) = service();
    assert!(svc.request(interaction(0)).await.is_err());
    assert_eq!(interaction(0).status, "pending");
}
