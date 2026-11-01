use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{Duration, Local};
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    ports::channel::NotifyChannel,
    ports::storage::TaskStore,
};
use ru41_plugin_library::{LibraryClient, LibraryConfig, LibraryTracker};
use std::{
    future::Future,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    pin::Pin,
    sync::{Arc, Mutex},
};

/// 本地 mock 图书馆 API：任意 POST 都返回固定 loanList JSON，记录请求头。
fn mock_library_api(payload: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let headers = Arc::new(Mutex::new(Vec::new()));
    let headers_clone = Arc::clone(&headers);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut head = String::new();
            loop {
                head.clear();
                if reader.read_line(&mut head).unwrap() == 0 {
                    break;
                }
                let line = head.to_lowercase();
                if let Some(value) = line.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if head.trim().is_empty() {
                    break;
                }
                headers_clone
                    .lock()
                    .unwrap()
                    .push(head.trim().to_owned());
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, headers)
}

/// 过期日期（今天 + offset 天）。
fn due_date(offset_days: i64) -> String {
    (Local::now().date_naive() + Duration::days(offset_days))
        .format("%Y-%m-%d")
        .to_string()
}

fn loan_json(offset_days: i64, title: &str) -> String {
    format!(
        r#"{{"title":"{title}","author":"某人","loanDate":"2026-10-01",
            "normReturnDate":"{due}","locationName":"计算机书库",
            "barcode":"T{t}","recordId":"rec{t}"}}"#,
        due = due_date(offset_days),
        t = title.len()
    )
}

/// 生成 payload.exp 在未来的测试 JWT。
fn make_jwt(exp_offset: i64) -> String {
    let payload = serde_json::json!({ "exp": ru41::util::now() + exp_offset, "sub": "test" });
    format!(
        "h.{}.s",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap())
    )
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
    api_base: &str,
    session_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    remind_days: i64,
) -> LibraryTracker {
    make_tracker_full(store, api_base, "http://localhost/cas", session_path, notifier, remind_days)
}

fn make_tracker_full(
    store: &Arc<dyn TaskStore>,
    api_base: &str,
    cas_base: &str,
    session_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    remind_days: i64,
) -> LibraryTracker {
    let config = LibraryConfig {
        username: "u".into(),
        password: "p".into(),
        cas_base: cas_base.to_owned(),
        cas_modulus: "0f".into(),
        cas_exponent: "010001".into(),
        unified_base: "http://localhost/unified".into(),
        find_base: "http://localhost/find".into(),
        mobile_base: api_base.to_owned(),
        group_code: "201550".into(),
        session_path,
        interval_secs: 3600,
        remind_days,
        notify_target: "default".into(),
    };
    let client = LibraryClient::new(
        "u",
        "p",
        config.cas_base.clone(),
        config.cas_modulus.clone(),
        config.cas_exponent.clone(),
        config.unified_base.clone(),
        config.find_base.clone(),
        config.mobile_base.clone(),
        config.group_code.clone(),
        config.session_path.clone(),
    );
    LibraryTracker::new(store.clone(), client, notifier, config)
}

fn seed_session(path: &std::path::Path, jwt: &str) {
    std::fs::write(
        path,
        serde_json::json!({ "jwt": jwt, "jwtHeader": "jwtOpacAuth" }).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn poll_alerts_due_soon_and_overdue_once_per_day() {
    let payload: &'static str = Box::leak(
        format!(
            r#"{{"success":true,"data":{{"searchResult":[{},{},{}],"loanNum":8,"numFound":3}}}}"#,
            loan_json(2, "临近到期书"),
            loan_json(-3, "已过期书"),
            loan_json(30, "远期书")
        )
        .into_boxed_str(),
    );
    let (api_base, headers) = mock_library_api(payload);
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.json");
    seed_session(&session_path, &make_jwt(3600));
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = make_tracker(&store, &api_base, session_path, Some(notifier), 3);

    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.loans, 3);
    // 只推 3 天窗口内的两本，远期书不打扰。
    assert_eq!(report.due_soon, vec!["临近到期书".to_owned()]);
    assert_eq!(report.overdue, vec!["已过期书".to_owned()]);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        let text = &sent[0];
        assert!(text.contains("## 📚 图书催还"));
        assert!(text.contains("临近到期书"));
        assert!(text.contains("还有 2 天"));
        assert!(text.contains("❌ 已过期 3 天"));
        assert!(!text.contains("远期书"));
    }
    // API 请求带了三件套头（http crate 发送时头名一律小写）。
    {
        let all_headers = headers.lock().unwrap().join("\n").to_lowercase();
        assert!(all_headers.contains("jwtopacauth:"));
        assert!(all_headers.contains("groupcode: 201550"));
        assert!(all_headers.contains("mappingpath:"));
    }

    // 第二轮：同日不重复提醒。
    let again = tracker.poll_once().await.unwrap();
    assert!(!again.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn quiet_library_sends_nothing() {
    let payload: &'static str = Box::leak(
        format!(
            r#"{{"success":true,"data":{{"searchResult":[{}],"loanNum":8,"numFound":1}}}}"#,
            loan_json(20, "远期书")
        )
        .into_boxed_str(),
    );
    let (api_base, _headers) = mock_library_api(payload);
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.json");
    seed_session(&session_path, &make_jwt(3600));
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = make_tracker(&store, &api_base, session_path, Some(notifier), 3);

    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert!(report.due_soon.is_empty());
    assert!(!report.notified);
    assert!(channel.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn expired_jwt_reports_login_failure_daily() {
    let (api_base, _headers) = mock_library_api(r#"{"success":true,"data":{"searchResult":[]}}"#);
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.json");
    // JWT 已过期 → ensure_session 触发完整登录 → cas_base 也指到 mock
    //（返回的不是验证码 JSON，登录链快速失败，不磨 30s 网络超时）。
    seed_session(&session_path, &make_jwt(-3600));
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = Arc::new(MockChannel::default());
    let notifier: Arc<dyn NotifyChannel> = channel.clone();
    let tracker = make_tracker_full(&store, &api_base, &api_base, session_path, Some(notifier), 3);

    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert!(sent[0].contains("图书馆登录态失效"));
    }

    // 同日第二轮不再重复。
    let again = tracker.poll_once().await.unwrap();
    assert!(!again.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

/// 路径分发的 find 公开检索 mock：unify/search 与 groupitems 各回各的，
/// 记录请求头用于断言 groupCode/mappingPath 两头。
fn mock_find_api() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let headers = Arc::new(Mutex::new(Vec::new()));
    let headers_clone = Arc::clone(&headers);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0;
            let mut request_line = String::new();
            let mut head = String::new();
            loop {
                head.clear();
                if reader.read_line(&mut head).unwrap() == 0 {
                    break;
                }
                if request_line.is_empty() {
                    request_line = head.trim().to_owned();
                }
                let line = head.to_lowercase();
                if let Some(value) = line.strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if head.trim().is_empty() {
                    break;
                }
                headers_clone
                    .lock()
                    .unwrap()
                    .push(head.trim().to_owned());
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).unwrap();
            let payload = if request_line.contains("unify/search") {
                r#"{"success":true,"errCode":200,"data":{"numFound":2,"searchResult":[
                    {"recordId":279213,"title":"深度学习 : 原理与应用实践","author":"张重生著",
                     "publisher":"电子工业出版社","publishYear":"2016","isbn":"978-7-121-30413-2",
                     "callNoOne":"TP273/248","physicalCount":3},
                    {"recordId":287129,"title":"机器人Python极客编程入门与实战","author":"Python极客团队著",
                     "publisher":"电子工业出版社","publishYear":"2017","isbn":"978-7-121-32292-1",
                     "callNo":["TP311.56/731"],"physicalCount":1}
                ]}}"#
            } else if request_line.contains("groupitems") {
                r#"{"success":true,"errCode":200,"data":[
                    {"callNo":"TP273/248","barcode":"C0001","locationName":"计算机书库",
                     "curLocationName":"计算机书库","processType":"在架"},
                    {"callNo":"TP273/248","barcode":"C0002","locationName":"计算机书库",
                     "curLocationName":"借还书区","processType":"借出"},
                    {"callNo":"TP273/248","barcode":"C0003","locationName":"广州校区样本书库",
                     "curLocationName":"广州校区样本书库","processType":"在架"}
                ]}"#
            } else {
                r#"{"success":false,"errCode":404,"message":"unknown path"}"#
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            )
            .unwrap();
        }
    });
    (base, headers)
}

#[tokio::test]
async fn search_books_uses_public_headers_and_parses() {
    let (find_base, headers) = mock_find_api();
    let dir = tempfile::tempdir().unwrap();
    let client = LibraryClient::new(
        "u",
        "p",
        "http://localhost/cas".into(),
        "0f".into(),
        "010001".into(),
        "http://localhost/unified".into(),
        find_base.clone(),
        find_base,
        "201550".into(),
        dir.path().join("session.json"),
    );

    // 检索：免登录也能用（session.json 不存在也不影响）。
    let page = client.search_books("深度学习", 1, 5).await.unwrap();
    assert_eq!(page.num_found, 2);
    assert_eq!(page.books.len(), 2);
    let first = &page.books[0];
    assert_eq!(first.record_id, 279213);
    assert_eq!(first.title, "深度学习 : 原理与应用实践");
    assert_eq!(first.author.as_deref(), Some("张重生著"));
    assert_eq!(first.call_no.as_deref(), Some("TP273/248"));
    assert_eq!(first.physical_count, 3);
    // callNoOne 缺失时回退 callNo[0]。
    assert_eq!(page.books[1].call_no.as_deref(), Some("TP311.56/731"));

    // 馆藏：在架 1/2（样本库不计可借）。
    let items = client.holdings(279213).await.unwrap();
    assert_eq!(items.len(), 3);
    assert!(items[0].is_on_shelf());
    assert!(!items[1].is_on_shelf());
    assert_eq!(items[1].cur_location_name.as_deref(), Some("借还书区"));
    // 样本库单册：在架但仅供阅览。
    assert!(items[2].is_on_shelf());
    assert!(items[2].is_sample());
    assert!(!items[0].is_sample());

    // 公开两头：groupCode + mappingPath=gzus；绝不带 jwt / cookie。
    {
        let all = headers.lock().unwrap().join("\n").to_lowercase();
        assert!(all.contains("groupcode: 201550"));
        assert!(all.contains("mappingpath: gzus"));
        assert!(!all.contains("jwtopacauth:"));
        assert!(!all.contains("cookie:"));
    }
}
