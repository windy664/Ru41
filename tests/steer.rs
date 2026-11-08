//! steer 插话链路：QQ 写入 steer 记录 → dispatcher 把超时未消费的孤儿转成新任务。

use ru41::{ports::storage::TaskStore as _, runtime::dispatcher::NotifyDispatcher};
use std::sync::Arc;

fn write_steer(
    store: &Arc<ru41::adapters::database::sqlite::SqliteTaskStore>,
    id: &str,
    created_at: i64,
) -> anyhow::Result<()> {
    store.put_document(
        "steer",
        id,
        None,
        &serde_json::json!({
            "run_id": "run-1-1",
            "task_id": 1,
            "text": "顺便帮我把上周的也查了",
            "status": "pending",
            "notify": {"channel": "qq", "target": "user:U1", "msg_id": "m1"},
            "created_at": created_at,
        }),
    )?;
    Ok(())
}

#[tokio::test]
async fn stale_steer_is_requeued_as_new_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("state.db"))
            .unwrap(),
    );
    std::mem::forget(dir);
    let now = ru41::util::now();
    write_steer(&store, "m-stale", now - 60).unwrap();
    // 新鲜插话（泵还没来得及消费）不能动。
    write_steer(&store, "m-fresh", now).unwrap();

    let dispatcher = NotifyDispatcher {
        store: store.clone(),
        channels: Vec::new(),
        batch: 50,
        interval: std::time::Duration::from_secs(2),
        silent_sources: Vec::new(),
    };
    let converted = dispatcher.convert_stale_steers().await.unwrap();
    assert_eq!(converted, 1);

    // 孤儿已转成任务：prompt 是插话原文，通知路由保留。
    let tasks = store.list(10).unwrap();
    let requeued = tasks
        .iter()
        .find(|task| task.request_key == "steer:requeue:m-stale")
        .expect("orphan steer should become a task");
    assert_eq!(requeued.prompt, "顺便帮我把上周的也查了");
    assert_eq!(
        requeued.notify.as_ref().map(|route| route.target.as_str()),
        Some("user:U1")
    );
    // 文档标记为 requeued，不会重复转换。
    let doc = store.document("steer", "m-stale").unwrap().unwrap();
    assert_eq!(doc.value["status"], "requeued");
    assert_eq!(dispatcher.convert_stale_steers().await.unwrap(), 0);
    // 新鲜的还在等泵。
    let fresh = store.document("steer", "m-fresh").unwrap().unwrap();
    assert_eq!(fresh.value["status"], "pending");
}
