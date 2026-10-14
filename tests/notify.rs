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
