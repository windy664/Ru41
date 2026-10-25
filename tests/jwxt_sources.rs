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
    modules::jwxt::{
        JwxtClient, JwxtConfig, JwxtSection, JwxtTracker, SectionClock as JwxtSectionClock,
    },
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
    /// 服务端会话是否有效（false 时 index_initMenu 重定向回登录页，
    /// 模拟已建立的 JSESSIONID 被服务端作废）。
    session_ok: Mutex<bool>,
}

impl FixtureState {
    fn new(timetable: String) -> Self {
        let state = Self::default();
        *state.timetable.lock().unwrap() = timetable;
        *state.login_ok.lock().unwrap() = true;
        *state.tgt_ok.lock().unwrap() = true;
        *state.session_ok.lock().unwrap() = true;
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
            get(|State(state): State<Arc<FixtureState>>| async move {
                if *state.session_ok.lock().unwrap() {
                    "menu".into_response()
                } else {
                    Redirect::to("/jwglxt/xtgl/login_slogin.html").into_response()
                }
            }),
        )
        .route(
            "/jwglxt/xtgl/login_slogin.html",
            get(|| async { "<html>登录</html>" }),
        )
        .route(
            "/kaptcha",
            get(|| async {
                // 真实样本（6*4=24）：让自动求解链路在测试中完整跑通
                let png = std::fs::read("tests/fixtures/jwxt_captcha/04.png").unwrap();
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png);
                Json(json!({
                    "uid": "cap-fixture",
                    "kaptchaType": "1",
                    "content": format!("data:image/png;base64,{b64}")
                }))
            }),
        )
        .route(
            "/jwglxt/kbcx/xskbcx_cxXsgrkb.html",
            post(
                |State(state): State<Arc<FixtureState>>, headers: axum::http::HeaderMap| async move {
                    let has_session = *state.session_ok.lock().unwrap()
                        && headers
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
    make_tracker_full(
        store,
        base,
        session_path,
        notifier,
        semester_start,
        daily_remind,
        false,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_tracker_full(
    store: Arc<dyn TaskStore>,
    base: &str,
    session_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    semester_start: chrono::NaiveDate,
    daily_remind: bool,
    sync_schedule: bool,
    section_times: Option<Vec<chrono::NaiveTime>>,
) -> JwxtTracker {
    let client = make_client(base, session_path.clone());
    JwxtTracker::new(
        store,
        client,
        notifier,
        JwxtConfig {
            cas_base: base.to_owned(),
            cas_modulus: JwxtSection::DEFAULT_CAS_MODULUS.to_owned(),
            cas_exponent: JwxtSection::DEFAULT_CAS_EXPONENT.to_owned(),
            jwxt_base: base.to_owned(),
            session_path,
            semester_start: Some(semester_start),
            daily_remind,
            remind_hour: 0, // 任意时刻都到点，测试可复现
            sync_schedule,
            clock: section_times.map(|times| JwxtSectionClock {
                times,
                duration_mins: 45,
            }),
            ..Default::default()
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

    // 无 TGT、无会话 → 自动完成验证码登录（solver 解 fixture 算术题），无需人工
    client.ensure_session().await.unwrap();
    assert_eq!(
        *state.login_hits.lock().unwrap(),
        1,
        "自动登录应触发一次账密 POST"
    );

    // 显式验证码登录（模拟 `jwxt login --captcha`）：表单里的密码必须是 Ohdave RSA hex
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
    assert_eq!(
        *state.login_hits.lock().unwrap(),
        2,
        "自动登录 1 次 + 手动 1 次"
    );

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
    assert_eq!(*state.login_hits.lock().unwrap(), 2);
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
async fn timetable_without_session_auto_logs_in() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let client = make_client(&format!("http://{addr}"), dir.path().join("session.json"));
    // 无任何会话/凭证缓存：timetable 应自动完成验证码登录后取到数据
    let meetings = client.timetable("2026", "3").await.unwrap();
    assert_eq!(meetings.len(), 2);
    assert_eq!(*state.login_hits.lock().unwrap(), 1);
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

/// 提醒与 QQ 回复显示具体钟点（不只"第几节"），且是 markdown 排版。
#[tokio::test]
async fn remind_and_describe_use_clock_times_and_markdown() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let base = format!("http://{addr}");
    let session_path = dir.path().join("session.json");
    pre_login(&base, &session_path).await;
    let tracker = make_tracker_full(
        store.clone(),
        &base,
        session_path,
        None,
        monday,
        false,
        false,
        Some(section_times_16()),
    );
    tracker.poll_once().await.unwrap();

    // 第2周星期三：fixture 里是 大学英语 3-4 节 → 10:40-12:00
    let target = monday + chrono::Duration::days(9);
    let clock = ru41::modules::jwxt::SectionClock {
        times: section_times_16(),
        duration_mins: 40,
    };
    let store_dyn: Arc<dyn ru41::ports::storage::TaskStore> = store.clone();
    let reply = ru41::modules::jwxt::describe_day_from_store(
        &store_dyn,
        Some(monday),
        Some(&clock),
        target,
    )
    .unwrap();
    // markdown 标题 + 每行课程带具体时间区间
    assert!(reply.starts_with("## 📅"), "{reply}");
    assert!(
        reply.contains("- **10:40-12:00** 大学英语 @B202（李四）"),
        "{reply}"
    );
    assert!(!reply.contains("节 大学英语"), "{reply}");
    assert!(reply.contains("共 1 节课"), "{reply}");

    // 未配置作息表时回退到节次文本
    let fallback =
        ru41::modules::jwxt::describe_day_from_store(&store_dyn, Some(monday), None, target)
            .unwrap();
    assert!(fallback.contains("- **3-4节** 大学英语"), "{fallback}");
}

/// 真实作息：8 大节 × 2 小节 × 40 分钟。
fn section_times_16() -> Vec<chrono::NaiveTime> {
    [
        "09:00", "09:40", "10:40", "11:20", "12:30", "13:10", "14:00", "14:40", "15:30", "16:10",
        "17:00", "17:40", "19:00", "19:40", "20:30", "21:10",
    ]
    .iter()
    .map(|h| chrono::NaiveTime::parse_from_str(h, "%H:%M").unwrap())
    .collect()
}

#[tokio::test]
async fn schedule_sync_creates_events_and_follows_changes() {
    use chrono::{NaiveTime, Timelike};
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
    // 16 节时刻表：第 N 节 = 08:00 + (N-1)*55min
    let section_times: Vec<NaiveTime> = (0..16)
        .map(|i| NaiveTime::from_hms_opt(8, 0, 0).unwrap() + chrono::Duration::minutes(i * 55))
        .collect();
    let tracker = make_tracker_full(
        store.clone(),
        &base,
        session_path,
        Some(channel.clone()),
        monday,
        false,
        true,
        Some(section_times),
    );

    let jwxt_events = |store: &SqliteTaskStore| {
        store
            .documents("event", 1000)
            .unwrap()
            .into_iter()
            .filter_map(|d| serde_json::from_value::<serde_json::Value>(d.value).ok())
            .filter(|v| v["source"]["channel"] == "jwxt" && v["status"] == "active")
            .collect::<Vec<_>>()
    };

    // 第一轮：semester_start=本周一 → 本周=第1周、下周=第2周。
    // 高数 1-16周（两周都有）+ 英语 2-17周（只有下周）→ 共 3 个事件
    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.events_created, 3);
    assert_eq!(report.events_cancelled, 0);
    let events = jwxt_events(&store);
    assert_eq!(events.len(), 3);
    // 校验一个事件的时间：高等数学 周二 1-2节 → 本周二 08:00
    let math = events.iter().find(|v| v["title"] == "高等数学").unwrap();
    let start = chrono::DateTime::from_timestamp(math["starts_at"].as_i64().unwrap(), 0)
        .unwrap()
        .with_timezone(&chrono::Local);
    assert_eq!(start.weekday(), chrono::Weekday::Tue);
    assert_eq!(start.time().hour(), 8);
    assert_eq!(math["location"], "A101");

    // 第二轮：幂等，零写入
    let report = tracker.poll_once().await.unwrap();
    assert_eq!((report.events_created, report.events_cancelled), (0, 0));

    // 调课：高数换教室 → 旧事件取消、新事件创建（本周+下周各一对）
    *state.timetable.lock().unwrap() = timetable_json("A102");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!((report.events_created, report.events_cancelled), (2, 2));
    let events = jwxt_events(&store);
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .filter(|v| v["title"] == "高等数学")
            .all(|v| v["location"] == "A102")
    );

    // 作息表配置调整（第 1 节从 08:00 改为 09:00）→ 同 source_id 但时间不同，
    // 旧事件必须取消重建
    let shifted: Vec<NaiveTime> = (0..16)
        .map(|i| NaiveTime::from_hms_opt(9, 0, 0).unwrap() + chrono::Duration::minutes(i * 55))
        .collect();
    let tracker2 = make_tracker_full(
        store.clone(),
        &base,
        dir.path().join("session.json"),
        None,
        monday,
        false,
        true,
        Some(shifted),
    );
    let report = tracker2.poll_once().await.unwrap();
    assert_eq!((report.events_created, report.events_cancelled), (3, 3));
    let events = jwxt_events(&store);
    let math = events.iter().find(|v| v["title"] == "高等数学").unwrap();
    let start = chrono::DateTime::from_timestamp(math["starts_at"].as_i64().unwrap(), 0)
        .unwrap()
        .with_timezone(&chrono::Local);
    assert_eq!(start.time().hour(), 9, "事件时间应跟随新的作息表");
}

#[tokio::test]
async fn expired_tgt_recovers_via_auto_login() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let today = chrono::Local::now().date_naive();
    let monday = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    // 会话文件里预置一个已过期的 TGT（fixture 只认 "TGT-test"，旧的会被拒）
    let session_path = dir.path().join("session.json");
    std::fs::write(&session_path, r#"{"cas_tgt":"TGT-expired","cookies":[]}"#).unwrap();
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        session_path,
        Some(channel.clone()),
        monday,
        false,
    );

    // 过期 TGT 换 ST 被拒 → 自动验证码登录恢复，全程无提醒
    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.meetings, 2);
    assert!(!report.notified);
    assert!(channel.sent.lock().unwrap().is_empty());
    assert_eq!(*state.login_hits.lock().unwrap(), 1, "应自动账密登录一次");
    // 持久化的新 TGT 已替换旧的
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("session.json")).unwrap())
            .unwrap();
    assert_eq!(saved["cas_tgt"], "TGT-test");
}

#[tokio::test]
async fn cas_hard_failure_alerts_once_per_day() {
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    *state.tgt_ok.lock().unwrap() = false;
    *state.login_ok.lock().unwrap() = false; // CAS 拒绝一切（模拟密码错误/锁定）
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
    assert!(channel.sent.lock().unwrap()[0].contains("教务登录失败"));

    // 同日第二轮：提醒去重
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn cas_failure_silent_while_timetable_cache_fresh() {
    // 先成功一轮建立学期课表缓存
    let state = Arc::new(FixtureState::new(timetable_json("A101")));
    let addr = spawn_fixture(state.clone()).await;
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
    assert!(report.login_ok);
    let baseline = channel.sent.lock().unwrap().len();

    // 服务端会话作废 + CAS 开始拒绝一切（验证码识别失效/密码错误等瞬时故障）：
    // 有缓存且未满 3 天，静默重试不打扰。
    *state.session_ok.lock().unwrap() = false;
    *state.tgt_ok.lock().unwrap() = false;
    *state.login_ok.lock().unwrap() = false;
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(!report.notified, "有课表缓存时单日故障不应提醒");
    assert_eq!(channel.sent.lock().unwrap().len(), baseline);
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), baseline);
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
