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

const BALANCE_JSON: &str = r#"{"ret":true,"msg":"查询成功","obj":{
  "roomNum":"2158","powerBalance":"8.5","waterBalance":"2.8",
  "schoolAreaNo":"81","buildingNo":"1859","schoolArea":"测试校区",
  "building":"测试楼A栋","room":"A#101",
  "formatPowerBalanceStr":"8.50度","formatWaterBalanceStr":"2.80吨"}}"#;

const HEALTHY_JSON: &str = r#"{"ret":true,"msg":"查询成功","obj":{
  "roomNum":"2158","powerBalance":"33.5","waterBalance":"3.4",
  "schoolAreaNo":"81","buildingNo":"1859","schoolArea":"测试校区",
  "building":"测试楼A栋","room":"A#101",
  "formatPowerBalanceStr":"33.50度","formatWaterBalanceStr":"3.40吨"}}"#;

/// 本地 mock ecard：返回固定余额，记录请求体。
fn mock_ecard(payload: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let bodies_clone = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if line.trim().is_empty() {
                    break;
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            bodies_clone
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&body).into_owned());
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, bodies)
}

#[derive(Default)]
struct MockChannel {
    sent: Mutex<Vec<String>>,
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
            self.sent.lock().unwrap().push(format!("{target}|{text}"));
            Ok(())
        })
    }
}

fn make_tracker(
    store: &Arc<dyn TaskStore>,
    base_url: &str,
    notifier: Option<Arc<dyn NotifyChannel>>,
    power_threshold: f64,
) -> EcardTracker {
    EcardTracker::new(
        store.clone(),
        EcardClient::with_base_url(base_url),
        notifier,
        EcardConfig {
            room: EcardRoom {
                impl_type: "CGCOMMON1111".into(),
                school_area_no: "81".into(),
                building_no: "1859".into(),
                room_num: "2158".into(),
            },
            token: "test-token".into(),
            power_threshold,
            notify_target: "default".into(),
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn poll_records_reading_and_alerts_below_threshold() {
    let (base, bodies) = mock_ecard(BALANCE_JSON);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = make_tracker(&store, &base, Some(notifier), 10.0);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.power, Some(8.5));
    assert_eq!(report.water, Some(2.8));
    assert_eq!(report.room_label, "测试楼A栋 A#101");
    assert!(report.alert_sent);
    assert!(report.alert_reason.unwrap().contains("8.50度"));

    // 请求体带 token 与房间参数。
    let body = bodies.lock().unwrap()[0].clone();
    assert!(body.contains("token=test-token"));
    assert!(body.contains("roomNum=2158"));

    // 读数与提醒都已落库。
    let today = ru41::util::today_utc(ru41::util::now());
    let reading = store.document("ecard_reading", &today).unwrap().unwrap();
    assert_eq!(reading.value["power"], 8.5);
    assert!(store.document("ecard_alert", &today).unwrap().is_some());

    // 第二轮：同日不重复提醒，但读数照常更新。
    let second = tracker.poll_once().await.unwrap();
    assert!(!second.alert_sent);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    let text = channel.sent.lock().unwrap()[0].clone();
    assert!(text.starts_with("default|## ⚡ 水电提醒"));
    assert!(text.contains("8.50度"));
}

#[tokio::test]
async fn no_alert_above_threshold() {
    let (base, _) = mock_ecard(BALANCE_JSON);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = make_tracker(&store, &base, Some(notifier), 5.0);

    let report = tracker.poll_once().await.unwrap();
    assert!(!report.alert_sent);
    assert_eq!(report.alert_reason, None);
    assert!(channel.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn daily_report_sends_once_per_day_when_enabled() {
    let (base, _bodies) = mock_ecard(HEALTHY_JSON);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = EcardTracker::new(
        store.clone(),
        EcardClient::with_base_url(&base),
        Some(notifier),
        EcardConfig {
            notify_target: "default".into(),
            daily_report: true,
            ..Default::default()
        },
    );

    // 首轮：健康读数也推日报。
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.alert_sent);
    assert!(report.report_sent);
    let text = channel.sent.lock().unwrap()[0].clone();
    assert!(text.starts_with("default|## ⚡ 水电日报"));
    assert!(text.contains("33.50 度"));

    // 同日第二轮：读数照常更新，日报不重复。
    let again = tracker.poll_once().await.unwrap();
    assert!(!again.report_sent);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    assert!(
        store
            .document("ecard_report", &ru41::util::today_utc(ru41::util::now()))
            .unwrap()
            .is_some()
    );

    // 未开启 daily_report 时不发送。
    let quiet = EcardTracker::new(
        store,
        EcardClient::with_base_url(&base),
        Some(Arc::new(MockChannel::default())),
        EcardConfig {
            notify_target: "default".into(),
            ..Default::default()
        },
    );
    assert!(!quiet.poll_once().await.unwrap().report_sent);
}

#[tokio::test]
async fn rejected_response_is_an_error() {
    let (base, _) = mock_ecard(r#"{"ret":false,"msg":"token已失效","obj":null}"#);
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(&store, &base, None, 10.0);
    let error = tracker.poll_once().await.unwrap_err();
    assert!(format!("{error:#}").contains("token已失效"));
}
use ru41_plugin_ecard::{EcardClient, EcardConfig, EcardRoom, EcardTracker};
