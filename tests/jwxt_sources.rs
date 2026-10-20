//! 教务课表监控的本地 mock 测试：CAS REST 登录链、课表解析、变更 diff、
//! 每日明日课表去重、登录失效提醒。不访问真实教务。

use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Form, Path as AxumPath, State},
    response::{AppendHeaders, IntoResponse, Redirect},
    routing::{get, post},
};
use chrono::Datelike;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    modules::jwxt::{JwxtClient, JwxtConfig, JwxtSection, JwxtTracker},
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use serde_json::json;

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

fn mock_channel() -> Arc<MockChannel> {
    Arc::new(MockChannel::default())
}

#[derive(Default)]
struct FixtureState {
    /// CAS 账密登录是否放行（false 时回 CODEFALSE，模拟验证码/密码错误）。
    login_ok: Mutex<bool>,
    /// TGT 是否有效（false 时换 ST 404，模拟 TGT 过期）。
    tgt_ok: Mutex<bool>,
    /// /v1/tickets 登录 POST 命中次数。
    login_hits: Mutex<u32>,
    /// /v1/tickets/{tgt} 换 ST 命中次数。
    st_hits: Mutex<u32>,
    /// 最近一次登录请求捕获的表单。
    last_login_form: Mutex<Vec<(String, String)>>,
    /// 课表接口返回的 JSON（外部可改写以模拟调课）。
    timetable: Mutex<String>,
}

impl FixtureState {
    fn new(timetable: String) -> Self {
        let state = Self::default();
        *state.timetable.lock().unwrap() = timetable;
        *state.login_ok.lock().unwrap() = true;
        *state.tgt_ok.lock().unwrap() = true;
        state
    }
}

fn timetable_json(course_room: &str) -> String {
    json!({
        "qsxqj": 1,
        "xsxx": {"XNM": "2026", "XQM": "3"},
        "kbList": [
            {"kcmc": "高等数学", "kch_id": "MATH1", "xm": "张三", "xqj": 2,
             "jc": "1-2节", "jcs": "1-2", "zcd": "1-16周", "cdmc": course_room,
             "kcxz": "必修", "jxbmc": "G1", "xnm": "2026", "xqm": "3"},
            {"kcmc": "大学英语", "kch_id": "ENG1", "xm": "李四", "xqj": 3,
             "jc": "3-4节", "jcs": "3-4", "zcd": "2-17周", "cdmc": "B202",
             "kcxz": "必修", "jxbmc": "G2", "xnm": "2026", "xqm": "3"},
            {"kcmc": "缺星期行", "xqj": "x"}
        ]
    })
    .to_string()
}

async fn spawn_fixture(state: Arc<FixtureState>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/v1/tickets",
            post(
                |State(state): State<Arc<FixtureState>>, Form(form): Form<Vec<(String, String)>>| async move {
                    *state.login_hits.lock().unwrap() += 1;
                    *state.last_login_form.lock().unwrap() = form;
                    if *state.login_ok.lock().unwrap() {
                        (
                            AppendHeaders([(
                                "set-cookie",
                                "CASTGC=TGT-test; Path=/",
                            )]),
                            Json(json!({"tgt": "TGT-test", "ticket": "ST-direct"})),
                        )
                            .into_response()
                    } else {
                        Json(json!({"meta": {"success": true}, "data": {"code": "CODEFALSE"}}))
                            .into_response()
                    }
                },
            ),
        )
        .route(
            "/v1/tickets/{tgt}",
            post(
                |State(state): State<Arc<FixtureState>>, AxumPath(tgt): AxumPath<String>| async move {
                    *state.st_hits.lock().unwrap() += 1;
                    if tgt == "TGT-test" && *state.tgt_ok.lock().unwrap() {
                        ("ST-fixture123").into_response()
                    } else {
                        (axum::http::StatusCode::NOT_FOUND, "no tgt").into_response()
                    }
                },
            ),
        )
        .route(
            "/sso/lyiotlogin",
            get(|| async {
                (
                    AppendHeaders([(
                        "set-cookie",
                        "JSESSIONID=fixture-session; Path=/",
                    )]),
                    Redirect::to("/jwglxt/ticketlogin?uid=fixture&verify=ff"),
                )
            }),
        )
        .route(
            "/jwglxt/ticketlogin",
            get(|| async { Redirect::to("/jwglxt/xtgl/index_initMenu.html") }),
        )
        .route(
            "/jwglxt/xtgl/index_initMenu.html",
            get(|| async { "menu" }),
        )
        .route(
            "/jwglxt/xtgl/login_slogin.html",
            get(|| async { "<html>登录</html>" }),
        )
        .route(
            "/kaptcha",
            get(|| async {
                // 1x1 透明 PNG（base64）
                Json(json!({
                    "uid": "cap-fixture",
                    "kaptchaType": "1",
                    "content": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
                }))
            }),
        )
        .route(
            "/jwglxt/kbcx/xskbcx_cxXsgrkb.html",
            post(
                |State(state): State<Arc<FixtureState>>, headers: axum::http::HeaderMap| async move {
                    let has_session = headers
                        .get("cookie")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|c| c.contains("JSESSIONID=fixture-session"));
                    if !has_session {
                        return Redirect::to("/jwglxt/xtgl/login_slogin.html").into_response();
                    }
                    (
                        [(axum::http::header::CONTENT_TYPE, "application/json;charset=UTF-8")],
                        state.timetable.lock().unwrap().clone(),
                    )
                        .into_response()
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn make_client(base: &str, session_path: std::path::PathBuf) -> JwxtClient {
    JwxtClient::new(
        "2640000001",
        "test-pass",
        base.to_owned(),
        JwxtSection::DEFAULT_CAS_MODULUS.to_owned(),
        JwxtSection::DEFAULT_CAS_EXPONENT.to_owned(),
        base.to_owned(),
        session_path,
    )
    .with_browser_ua()
}

fn make_tracker(
    store: Arc<dyn TaskStore>,
    base: &str,
    session_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    semester_start: chrono::NaiveDate,
    daily_remind: bool,
) -> JwxtTracker {
    let client = make_client(base, session_path.clone());
    JwxtTracker::new(
        store,
        client,
        notifier,
        JwxtConfig {
            username: "2640000001".into(),
            password: "test-pass".into(),
            cas_base: base.to_owned(),
            cas_modulus: JwxtSection::DEFAULT_CAS_MODULUS.to_owned(),
            cas_exponent: JwxtSection::DEFAULT_CAS_EXPONENT.to_owned(),
            jwxt_base: base.to_owned(),
            session_path,
            semester_start: Some(semester_start),
            xnm: None,
            xqm: None,
            interval_secs: 60,
            daily_remind,
            remind_hour: 0, // 任意时刻都到点，测试可复现
            notify_target: "user:mock".into(),
        },
    )
}

#[tokio::test]
async fn cas_login_then_session_reuse_and_timetable() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let session_path = dir.path().join("session.json");
    let client = make_client(&format!("http://{addr}"), session_path.clone());

    // 无 TGT、无会话 → 需要人工验证码登录
    let err = client.ensure_session().await.unwrap_err();
    assert!(err.to_string().contains("验证码"), "got: {err}");

    // 验证码登录：表单里的密码必须是 Ohdave RSA hex，而不是明文
    client.cas_login("cap-uid", "56").await.unwrap();
    {
        let form = state.last_login_form.lock().unwrap();
        let get = |key: &str| {
            form.iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("username"), "2640000001");
        assert_eq!(get("id"), "cap-uid");
        assert_eq!(get("code"), "56");
        assert!(
            get("password")
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ' '),
            "password must be ohdave hex, got {}",
            get("password")
        );
        assert_eq!(get("service"), format!("http://{addr}/sso/lyiotlogin"));
    }
    assert_eq!(*state.login_hits.lock().unwrap(), 1);

    // TGT → ST → 教务会话
    client.ensure_session().await.unwrap();
    let meetings = client.timetable("2026", "3").await.unwrap();
    assert_eq!(meetings.len(), 2, "缺星期行应跳过");
    assert_eq!(meetings[0].course, "高等数学");
    assert_eq!(meetings[0].classroom, "A101");
    assert_eq!(meetings[0].weeks, (1..=16).collect::<Vec<_>>());
    assert_eq!(meetings[1].xqm, "3");

    // 新客户端实例（新 cookie jar）复用持久化 TGT：不再触发账密登录 POST
    let client2 = make_client(&format!("http://{addr}"), session_path);
    client2.ensure_session().await.unwrap();
    assert_eq!(*state.login_hits.lock().unwrap(), 1);
    assert_eq!(*state.st_hits.lock().unwrap(), 2);
}

#[tokio::test]
async fn captcha_download_creates_dirs_and_stores_uid() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    // 会话目录尚不存在：download_captcha 必须自建父目录（首次部署的 `jwxt login`）
    let session_path = dir.path().join("fresh/session.json");
    let client = make_client(&format!("http://{addr}"), session_path);
    let (uid, path) = client.download_captcha().await.unwrap();
    assert_eq!(uid, "cap-fixture");
    assert!(path.exists());
    assert_eq!(client.pending_captcha_uid().as_deref(), Some("cap-fixture"));
}

#[tokio::test]
async fn cas_rejection_surfaces_code() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    *state.login_ok.lock().unwrap() = false;
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(&format!("http://{addr}"), dir.path().join("session.json"));
    let err = client.cas_login("cap-uid", "56").await.unwrap_err();
    assert!(err.to_string().contains("CODEFALSE"), "got: {err}");
}

#[tokio::test]
async fn timetable_without_session_is_login_required() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(&format!("http://{addr}"), dir.path().join("session.json"));
    let err = client.timetable("2026", "3").await.unwrap_err();
    assert!(
        matches!(err, ru41::modules::jwxt::JwxtError::LoginRequired),
        "got: {err}"
    );
}

/// 模拟 `ru41 jwxt login`：先用同一会话文件完成一次验证码登录，
/// tracker 轮询才有 TGT 可复用（模块按设计不做无人值守账密登录）。
async fn pre_login(base: &str, session_path: &std::path::Path) {
    let client = make_client(base, session_path.to_owned());
    client.cas_login("cap-uid", "56").await.unwrap();
}

#[tokio::test]
async fn tracker_poll_baselines_then_diffs_changes() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let base = format!("http://{addr}");
    let session_path = dir.path().join("session.json");
    pre_login(&base, &session_path).await;
    let tracker = make_tracker(
        store.clone(),
        &base,
        session_path,
        Some(channel.clone()),
        monday,
        false,
    );

    // 第一轮：基线落库，不提醒
    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.meetings, 2);
    assert_eq!(report.changes, 0);
    assert!(!report.notified);
    assert!(
        store
            .document("jwxt_timetable", "2026#3")
            .unwrap()
            .is_some()
    );

    // 第二轮：换教室 → 变更提醒
    *state.timetable.lock().unwrap() = timetable_json("A102");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.changes, 2, "换教室=旧签名取消+新签名新增");
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("课表变更"));
        assert!(sent[0].contains("A102"));
    }

    // 第三轮：无变化
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.changes, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn daily_remind_is_idempotent_per_day() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let base = format!("http://{addr}");
    let session_path = dir.path().join("session.json");
    pre_login(&base, &session_path).await;
    let tracker = make_tracker(
        store.clone(),
        &base,
        session_path,
        Some(channel.clone()),
        monday,
        true,
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert!(report.daily_reminded);
    {
        let sent = channel.sent.lock().unwrap();
        assert!(sent.iter().any(|m| m.contains("明日课表")));
    }

    // 同日第二轮：不再推
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.daily_reminded);
    let daily_count = channel
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.contains("明日课表"))
        .count();
    assert_eq!(daily_count, 1);
    assert!(
        store
            .document("jwxt_alert", &format!("daily:{today}"))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn expired_tgt_alerts_once_per_day() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    *state.tgt_ok.lock().unwrap() = false;
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        dir.path().join("session.json"),
        Some(channel.clone()),
        monday,
        false,
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(report.notified);
    assert!(channel.sent.lock().unwrap()[0].contains("登录态失效"));

    // 同日第二轮：提醒去重
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[test]
fn toml_section_rejects_unknown_fields() {
    let toml_text = "username = \"u\"\npassword = \"p\"\nno_such_key = true\n";
    let section: Result<JwxtSection, _> = toml::from_str(toml_text);
    assert!(section.is_err(), "未知字段必须被拒绝");
    assert!(
        section.unwrap_err().to_string().contains("no_such_key"),
        "报错应指出未知字段名"
    );
    let ok: JwxtSection = toml::from_str("username = \"u\"\n").unwrap();
    assert_eq!(ok.username.as_deref(), Some("u"));
}
