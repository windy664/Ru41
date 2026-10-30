use ru41::{
    adapters::database::sqlite::SqliteTaskStore, ports::channel::NotifyChannel,
    ports::storage::TaskStore,
};
use std::{
    future::Future,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{Arc, Mutex},
};

const P40_UNAVAILABLE: &str = r#"{"Action":"CheckCompShareResourceCapacityResponse","RetCode":0,"Specs":[{"Cpu":8,"Gpu":1,"Mem":64,"ResourceEnough":false},{"Cpu":16,"Gpu":2,"Mem":64,"ResourceEnough":false}]}"#;
const P40_AVAILABLE: &str = r#"{"Action":"CheckCompShareResourceCapacityResponse","RetCode":0,"Specs":[{"Cpu":8,"Gpu":1,"Mem":64,"ResourceEnough":true},{"Cpu":16,"Gpu":2,"Mem":64,"ResourceEnough":false}]}"#;
const AUTH_EXPIRED: &str = r#"{"RetCode":170,"Message":"Missing signature"}"#;

type RecordedRequests = Arc<Mutex<Vec<(String, String)>>>;

fn mock_api(payloads: Vec<&'static str>) -> (String, RecordedRequests) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_clone = Arc::clone(&requests);
    std::thread::spawn(move || {
        for (index, stream) in listener.incoming().enumerate() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut headers = Vec::new();
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                if line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                headers.push(line.trim().to_owned());
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            requests_clone
                .lock()
                .unwrap()
                .push((headers.join("\n"), String::from_utf8(body).unwrap()));
            let payload = payloads
                .get(index)
                .copied()
                .unwrap_or_else(|| payloads.last().copied().unwrap());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, requests)
}

#[derive(Default)]
struct MockChannel {
    sent: Mutex<Vec<String>>,
    fail_next: Mutex<bool>,
}

impl NotifyChannel for MockChannel {
    fn channel_id(&self) -> &str {
        "mock"
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        _msg_id: Option<&'a str>,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
                anyhow::bail!("mock notify failed");
            }
            self.sent.lock().unwrap().push(format!("{target}|{text}"));
            Ok(())
        })
    }
}

fn tracker(
    base_url: &str,
    notifier: Option<Arc<dyn NotifyChannel>>,
) -> (CompshareTracker, Arc<dyn TaskStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = CompshareTracker::new(
        store.clone(),
        CompshareClient::new(base_url),
        notifier,
        CompshareConfig {
            base_url: base_url.into(),
            project_id: "org-test".into(),
            cookie: "U_USER_EMAIL=user%40example.edu.cn; U_CSRF_TOKEN=csrf; U_JWT_TOKEN=jwt".into(),
            notify_target: "user:U1".into(),
            ..Default::default()
        },
    );
    (tracker, store, dir)
}

#[tokio::test]
async fn first_available_poll_sends_detailed_notification_once() {
    let (base, requests) = mock_api(vec![P40_AVAILABLE, P40_AVAILABLE]);
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let (tracker, _, _dir) = tracker(&base, Some(notifier));

    let first = tracker.poll_once().await.unwrap();
    assert_eq!(first.state, "available");
    assert_eq!(first.available_specs, 1);
    assert!(first.notified);

    let second = tracker.poll_once().await.unwrap();
    assert!(!second.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    let text = &channel.sent.lock().unwrap()[0];
    assert!(text.starts_with("user:U1|## 🎮 恒源云 GPU 有货"));
    assert!(text.contains("1×GPU / 8C / 64GB"));
    assert!(text.contains("compshareImage-1t1vuerl80np"));

    let (headers, body) = &requests.lock().unwrap()[0];
    let lower_headers = headers.to_lowercase();
    assert!(lower_headers.contains("cookie: u_user_email=user%40example.edu.cn"));
    assert!(lower_headers.contains("u-csrf-token: csrf"));
    assert!(body.contains("GPUType=P40"));
    assert!(body.contains("Region=cn-wlcb"));
    assert!(body.contains("Zone=cn-wlcb-01"));
    assert!(body.contains("_user=user%40example.edu.cn"));
}

#[tokio::test]
async fn unavailable_to_available_transition_notifies() {
    let (base, _) = mock_api(vec![P40_UNAVAILABLE, P40_AVAILABLE, P40_AVAILABLE]);
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let (tracker, store, _dir) = tracker(&base, Some(notifier));

    let first = tracker.poll_once().await.unwrap();
    assert_eq!(first.state, "unavailable");
    assert!(!first.notified);
    assert!(channel.sent.lock().unwrap().is_empty());

    let second = tracker.poll_once().await.unwrap();
    assert_eq!(second.state, "available");
    assert!(second.notified);

    let third = tracker.poll_once().await.unwrap();
    assert!(!third.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    let doc = store
        .document(
            "compshare_state",
            "compshareImage-1t1vuerl80np:cn-wlcb:cn-wlcb-01:P40",
        )
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["state"], "available");
    assert!(doc.value["notified_at"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn notify_failure_keeps_transition_pending() {
    let (base, _) = mock_api(vec![P40_AVAILABLE, P40_AVAILABLE]);
    let channel = Arc::new(MockChannel::default());
    *channel.fail_next.lock().unwrap() = true;
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let (tracker, _, _dir) = tracker(&base, Some(notifier));

    let error = tracker.poll_once().await.unwrap_err();
    assert!(format!("{error:#}").contains("mock notify failed"));
    let second = tracker.poll_once().await.unwrap();
    assert!(second.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn expired_session_alerts_once_and_does_not_update_stock_state() {
    let (base, _) = mock_api(vec![AUTH_EXPIRED, AUTH_EXPIRED]);
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let (tracker, store, _dir) = tracker(&base, Some(notifier));

    let first = tracker.poll_once().await.unwrap();
    assert!(first.auth_expired);
    assert!(first.auth_alerted);
    let second = tracker.poll_once().await.unwrap();
    assert!(second.auth_expired);
    assert!(!second.auth_alerted);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    assert!(
        store
            .document(
                "compshare_state",
                "compshareImage-1t1vuerl80np:cn-wlcb:cn-wlcb-01:P40",
            )
            .unwrap()
            .is_none()
    );
}
use ru41_plugin_compshare::{CompshareClient, CompshareConfig, CompshareTracker};
