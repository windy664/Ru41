use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{AgentResult, NotifyRoute, RunContract, TaskStatus},
    ports::channel::NotifyChannel,
    ports::storage::TaskStore,
    runtime::dispatcher::NotifyDispatcher,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(Default)]
struct MockChannel {
    id: &'static str,
    sent: Mutex<Vec<(String, Option<String>, String)>>,
    fail: bool,
}

impl NotifyChannel for MockChannel {
    fn channel_id(&self) -> &str {
        self.id
    }
    fn send<'a>(
        &'a self,
        target: &'a str,
        msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if self.fail {
                anyhow::bail!("channel down");
            }
            self.sent.lock().unwrap().push((
                target.to_owned(),
                msg_id.map(str::to_owned),
                text.to_owned(),
            ));
            Ok(())
        })
    }
}

fn store_with_terminal_task(dir: &tempfile::TempDir, route: NotifyRoute) -> Arc<dyn TaskStore> {
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "qq:msg:m1",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap()
        .task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store
        .succeed(
            &claim,
            &AgentResult {
                summary: "处理完成".into(),
            },
            None,
            101,
        )
        .unwrap();
    assert_eq!(
        store.get(task.id).unwrap().unwrap().status,
        TaskStatus::Succeeded
    );
    store
}

fn dispatcher(
    store: Arc<dyn TaskStore>,
    channels: Vec<Arc<dyn NotifyChannel>>,
) -> NotifyDispatcher {
    NotifyDispatcher {
        store,
        channels,
        batch: 10,
        interval: Duration::from_secs(60),
    }
}

#[tokio::test]
async fn dispatches_terminal_result_to_matching_channel_once() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "group:G1".into(),
        msg_id: Some("m1".into()),
    };
    let store = store_with_terminal_task(&dir, route);
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let other = Arc::new(MockChannel {
        id: "feishu",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone(), other.clone()]);
    assert_eq!(d.run_once().await.unwrap(), 1);
    assert_eq!(d.run_once().await.unwrap(), 0);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "group:G1");
    assert_eq!(sent[0].1.as_deref(), Some("m1"));
    // 终态回复只有原生结果，不加脚手架页脚。
    assert_eq!(sent[0].2, "处理完成");
    assert!(other.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_send_is_not_marked_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m2".into()),
    };
    let store = store_with_terminal_task(&dir, route);
    let qq = Arc::new(MockChannel {
        id: "qq",
        fail: true,
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq]);
    assert_eq!(d.run_once().await.unwrap(), 0);
    assert_eq!(store.list_unnotified(10).unwrap().len(), 1);
}

#[tokio::test]
async fn task_without_registered_channel_is_left_pending() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "dingtalk".into(),
        target: "user:U1".into(),
        msg_id: None,
    };
    let store = store_with_terminal_task(&dir, route);
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq]);
    assert_eq!(d.run_once().await.unwrap(), 0);
    assert_eq!(store.list_unnotified(10).unwrap().len(), 1);
}

#[tokio::test]
async fn delivery_record_prevents_double_send_across_dispatchers() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "group:G9".into(),
        msg_id: Some("m9".into()),
    };
    let store = store_with_terminal_task(&dir, route);
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let a = dispatcher(store.clone(), vec![qq.clone()]);
    a.run_once().await.unwrap();
    // 另一个 dispatcher 实例处理同一批：delivery 记录为 sent，不得重发。
    let b = dispatcher(store.clone(), vec![qq.clone()]);
    b.run_once().await.unwrap();
    assert_eq!(qq.sent.lock().unwrap().len(), 1);
    let doc = store.document("delivery", "task-1-1").unwrap().unwrap();
    assert_eq!(doc.value["status"], "sent");
}

/// 运行中任务的 started 进度文档会送达会话，且只送一次。
#[tokio::test]
async fn dispatches_started_progress_once() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "qq:msg:m1",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap()
        .task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    // 模拟 worker 认领后写入的 started 进度文档。
    store
        .put_document(
            "progress",
            "run-1-1-started",
            None,
            &serde_json::json!({
                "task_id": task.id,
                "attempt_id": claim.attempt_id,
                "kind": "started",
                "status": "pending",
                "backend_id": "codex",
            }),
        )
        .unwrap();
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone()]);
    assert_eq!(d.run_once().await.unwrap(), 1);
    assert_eq!(d.run_once().await.unwrap(), 0);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.as_deref(), Some("m1"));
    assert!(sent[0].2.contains("开始执行"), "text: {}", sent[0].2);
    assert!(sent[0].2.contains("底座 `codex`"), "text: {}", sent[0].2);
    let doc = store
        .document("progress", "run-1-1-started")
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["status"], "sent");
}

/// 首次失败重试的 retry 进度文档带错误原因送达。
#[tokio::test]
async fn dispatches_retry_progress_with_reason() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "qq:msg:m1",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap()
        .task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store
        .fail(
            &claim,
            "Codex returned an unexecuted tool call",
            101,
            60,
            false,
            true,
        )
        .unwrap();
    store
        .put_document(
            "progress",
            "run-1-1-retry",
            None,
            &serde_json::json!({
                "task_id": task.id,
                "attempt_id": claim.attempt_id,
                "kind": "retry",
                "status": "pending",
                "error": "Codex returned an unexecuted tool call",
            }),
        )
        .unwrap();
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone()]);
    assert_eq!(d.run_once().await.unwrap(), 1);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].2.contains("首次执行失败"), "text: {}", sent[0].2);
    assert!(
        sent[0].2.contains("unexecuted tool call"),
        "text: {}",
        sent[0].2
    );
}

/// 活动流 chunk 按序送达会话；任务终态后剩余 chunk 直接消化。
#[tokio::test]
async fn dispatches_activity_chunks_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let task = store
        .enqueue_with_notify(
            "qq:msg:m1",
            "hello",
            "codex",
            RunContract::AnalysisV1,
            &route,
        )
        .unwrap()
        .task;
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    for (seq, lines) in [
        (0, vec!["💭 先分析需求"]),
        (1, vec!["⚙️ $ ls -la", "💬 目录如下"]),
    ] {
        store
            .put_document(
                "activity",
                &format!("run-1-1-a{seq:03}"),
                None,
                &serde_json::json!({
                    "task_id": task.id,
                    "attempt_id": claim.attempt_id,
                    "run_id": "run-1-1",
                    "seq": seq,
                    "lines": lines,
                    "status": "pending",
                }),
            )
            .unwrap();
    }
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone()]);
    assert_eq!(d.run_once().await.unwrap(), 2);
    assert_eq!(d.run_once().await.unwrap(), 0);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].2, "💭 先分析需求");
    assert_eq!(sent[1].2, "⚙️ $ ls -la\n💬 目录如下");
    let doc = store.document("activity", "run-1-1-a000").unwrap().unwrap();
    assert_eq!(doc.value["status"], "sent");
}

/// 任务终态后残留的 activity chunk 不再补发。
#[tokio::test]
async fn stale_activity_for_terminal_task_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };
    let store = store_with_terminal_task(&dir, route);
    store
        .put_document(
            "activity",
            "run-1-1-a000",
            None,
            &serde_json::json!({
                "task_id": 1,
                "attempt_id": 1,
                "run_id": "run-1-1",
                "seq": 0,
                "lines": ["💭 迟到的事件"],
                "status": "pending",
            }),
        )
        .unwrap();
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone()]);
    assert_eq!(d.run_once().await.unwrap(), 1);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].2.starts_with("处理完成"));
    let doc = store.document("activity", "run-1-1-a000").unwrap().unwrap();
    assert_eq!(doc.value["status"], "done");
}

/// 任务已终态时 started 进度不再补发，直接消化。
#[tokio::test]
async fn stale_progress_for_terminal_task_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };
    let store = store_with_terminal_task(&dir, route);
    store
        .put_document(
            "progress",
            "run-1-1-started",
            None,
            &serde_json::json!({
                "task_id": 1,
                "attempt_id": 1,
                "kind": "started",
                "status": "pending",
                "backend_id": "codex",
            }),
        )
        .unwrap();
    let qq = Arc::new(MockChannel {
        id: "qq",
        ..Default::default()
    });
    let d = dispatcher(store.clone(), vec![qq.clone()]);
    // 只发终态通知；过期的 started 进度被消化不重发。
    assert_eq!(d.run_once().await.unwrap(), 1);
    let sent = qq.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].2.starts_with("处理完成"));
    let doc = store
        .document("progress", "run-1-1-started")
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["status"], "done");
}
