use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{RawQuery, State},
    response::IntoResponse,
    routing::{get, post},
};
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    modules::teachermate::{
        TeachermateClient, TeachermateConfig, TeachermateSection, TeachermateTracker,
        TeachermateWorkTracker,
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

/// fixture 服务状态：待签列表/签到响应/学籍可控，记录签到命中与请求头。
#[derive(Default)]
struct FixtureState {
    /// active_signs 响应体（数组 JSON 或失效 message）。
    signs: Mutex<String>,
    /// student-sign-in 响应体（默认名次成功，可注入失败文案/失效 message）。
    sign_response: Mutex<String>,
    /// student-sign-in 命中次数。
    sign_hits: Mutex<usize>,
    /// 最后一次签到请求的 JSON body。
    last_sign_body: Mutex<serde_json::Value>,
    /// 最后一次请求带的 Openid 头。
    last_openid: Mutex<String>,
    /// /students/role 是否放行；Default 手写为 true，失效场景显式置 false。
    role_ok: Mutex<bool>,
    /// 失效模式：active_signs 返回 401 + 特征串（2026-10-24 实测行为）。
    expired: Mutex<bool>,
    /// 作业课程列表响应体（空数组 = 无作业课程）。
    hw_courses: Mutex<String>,
    /// courseId → 单课作业清单响应体。
    hw_lists: Mutex<std::collections::BTreeMap<i64, String>>,
}

impl FixtureState {
    fn new(signs: String) -> Self {
        Self {
            signs: Mutex::new(signs),
            sign_response: Mutex::new(r#"{"signRank":2,"studentRank":7}"#.into()),
            sign_hits: Mutex::new(0),
            last_sign_body: Mutex::new(serde_json::Value::Null),
            last_openid: Mutex::new(String::new()),
            role_ok: Mutex::new(true),
            expired: Mutex::new(false),
            hw_courses: Mutex::new(json!([]).to_string()),
            hw_lists: Mutex::new(std::collections::BTreeMap::new()),
        }
    }
}

/// 两个进行中签到：普通 + GPS（不同课程）。
fn signs_pending_json() -> String {
    json!([
        {"courseId": 101, "signId": 1001, "isGPS": 0, "isQR": 0,
         "name": "大学英语", "code": "CLS101"},
        {"courseId": 202, "signId": 2002, "isGPS": 1, "isQR": 0,
         "name": "高等数学", "code": "CLS202"}
    ])
    .to_string()
}

/// 一个进行中的二维码签到。
fn signs_qr_json() -> String {
    json!([
        {"courseId": 303, "signId": 3003, "isGPS": 0, "isQR": 1,
         "name": "线性代数", "code": "CLS303"}
    ])
    .to_string()
}

/// 空的进行中列表（全部签到已结束）。
fn signs_empty_json() -> String {
    json!([]).to_string()
}

async fn spawn_fixture(state: Arc<FixtureState>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/wechat-api/v1/class-attendance/student/active_signs",
            get(
                |State(state): State<Arc<FixtureState>>, headers: axum::http::HeaderMap| async move {
                    *state.last_openid.lock().unwrap() = headers
                        .get("openid")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    if *state.expired.lock().unwrap() {
                        return (
                            axum::http::StatusCode::UNAUTHORIZED,
                            Json(json!({"message": "登录信息失效，请退出后重试"})),
                        )
                            .into_response();
                    }
                    (
                        axum::http::StatusCode::OK,
                        state.signs.lock().unwrap().clone(),
                    )
                        .into_response()
                },
            ),
        )
        .route(
            "/wechat-api/v1/class-attendance/student-sign-in",
            post(
                |State(state): State<Arc<FixtureState>>, Json(body): Json<serde_json::Value>| async move {
                    *state.last_sign_body.lock().unwrap() = body;
                    *state.sign_hits.lock().unwrap() += 1;
                    state.sign_response.lock().unwrap().clone()
                },
            ),
        )
        .route(
            "/wechat-api/v2/students/role",
            get(|State(state): State<Arc<FixtureState>>| async move {
                if *state.role_ok.lock().unwrap() {
                    (
                        axum::http::StatusCode::OK,
                        Json(json!([{
                            "id": 1, "student_id": 9, "college_id": 1,
                            "class_name": "计科2401", "specialty": "", "student_number": "20249999",
                            "comment": null, "deleted": 0, "org_id": null,
                            "college_name": "某某大学", "department_name": "计算机学院"
                        }])),
                    )
                        .into_response()
                } else {
                    (
                        axum::http::StatusCode::UNAUTHORIZED,
                        Json(json!({"message": "登录信息失效，请退出后重试"})),
                    )
                        .into_response()
                }
            }),
        )
        .route(
            "/wechat-api/v1/homework/courses",
            get(|State(state): State<Arc<FixtureState>>| async move {
                (
                    axum::http::StatusCode::OK,
                    state.hw_courses.lock().unwrap().clone(),
                )
                    .into_response()
            }),
        )
        .route(
            "/wechat-api/v1/homework",
            get(
                |State(state): State<Arc<FixtureState>>,
                 RawQuery(query): RawQuery| async move {
                    let course_id = query.and_then(|q| {
                        q.split('&').find_map(|kv| {
                            kv.strip_prefix("courseId=")
                                .and_then(|v| v.parse::<i64>().ok())
                        })
                    });
                    let body = course_id.and_then(|id| {
                        state.hw_lists.lock().unwrap().get(&id).cloned()
                    });
                    (
                        axum::http::StatusCode::OK,
                        body.unwrap_or_else(|| json!([]).to_string()),
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

fn make_tracker(
    store: Arc<dyn TaskStore>,
    base: &str,
    openid: &str,
    notifier: Option<Arc<dyn NotifyChannel>>,
    auto_sign: bool,
    default_location: Option<(f64, f64)>,
) -> TeachermateTracker {
    let client = TeachermateClient::with_api_base(openid, base.to_owned());
    TeachermateTracker::new(
        store,
        client,
        notifier,
        TeachermateConfig {
            openid: openid.into(),
            openid_path: std::path::PathBuf::from("unused-openid"),
            interval_secs: 60,
            notify_target: "user:mock".into(),
            auto_sign,
            default_location,
            work_poll_secs: 1800,
            work_due_days: 3,
            work_todo_sync: false,
            work_vault_sync: false,
            vault_course_dir: None,
            vault_courses: Default::default(),
            work_vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
        },
    )
}

#[tokio::test]
async fn notifies_pending_once_then_reports_end() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-test-0001",
        Some(channel.clone()),
        false,
        None,
    );

    // Openid 头必须带上（协议核心：Header 鉴权）
    let report = tracker.poll_once().await.unwrap();
    assert!(report.openid_ok);
    assert_eq!(report.active_signs, 2);
    assert_eq!(report.new_signs, 2);
    assert!(report.notified);
    assert_eq!(
        state.last_openid.lock().unwrap().as_str(),
        "openid-test-0001"
    );
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("微助教签到"));
        assert!(sent[0].contains("大学英语"));
        assert!(sent[0].contains("位置签到"));
        assert!(sent[0].contains("teachermate sign 1001"));
    }
    assert_eq!(
        store
            .document("teachermate_sign", "101#1001")
            .unwrap()
            .map(|d| d
                .value
                .get("course")
                .and_then(|v| v.as_str())
                .map(str::to_owned)),
        Some(Some("大学英语".into()))
    );

    // 第二轮：活动仍在但 records 已见 → 不重复提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_signs, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);

    // 第三轮：活动从进行中列表消失且本侧无签到记录 → 🏁 结束回执
    *state.signs.lock().unwrap() = signs_empty_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.active_signs, 0);
    assert_eq!(report.ended_unsigned, 2);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("签到回执"));
        assert!(sent[1].contains("已结束"));
    }

    // 第四轮：结束状态已收敛 → 不再重复播报
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.ended_unsigned, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn auto_sign_signs_normal_and_gps_with_coords() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-test-0002",
        Some(channel.clone()),
        true,
        Some((30.5, 114.3)),
    );

    // 首轮：普通+GPS 当场自动签 → ✅ 回执，无 ⏰ 提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 2);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("自动签到成功"));
        assert!(!sent[0].contains("⏰"));
    }
    // GPS 活动带配置坐标；普通活动不带
    let gps_hits = store
        .documents("teachermate_sign", 10)
        .unwrap()
        .into_iter()
        .filter(|d| d.value["is_gps"] == 1)
        .count();
    assert_eq!(gps_hits, 1);

    // 快照已标记 signed_via=auto → 活动消失时不再播结束回执
    *state.signs.lock().unwrap() = signs_empty_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.ended_unsigned, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    let snap = store
        .document("teachermate_sign", "202#2002")
        .unwrap()
        .unwrap();
    assert_eq!(snap.value["signed_via"], "auto");
}

#[tokio::test]
async fn auto_sign_gps_payload_carries_default_coords() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        "openid-test-0003",
        None,
        true,
        Some((30.5, 114.3)),
    );
    tracker.poll_once().await.unwrap();
    // 两个活动都发起了签到；GPS 的载荷带 lat/lon
    assert_eq!(*state.sign_hits.lock().unwrap(), 2);
    let body = state.last_sign_body.lock().unwrap().clone();
    assert!(body.get("lat").is_some() || body.get("lon").is_some());
}

#[tokio::test]
async fn qr_sign_is_never_auto_signed_and_reminds_wechat_scan() {
    let state = Arc::new(FixtureState::new(signs_qr_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-test-0004",
        Some(channel.clone()),
        true,
        None,
    );

    // 二维码不进自动签：推送提醒并指路微信现场扫码，不发 sign 请求
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    assert_eq!(*state.sign_hits.lock().unwrap(), 0);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("二维码签到"));
        assert!(sent[0].contains("现场扫码"));
    }

    // 结束后照常给 🏁 回执（本侧无签到记录）
    *state.signs.lock().unwrap() = signs_empty_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.ended_unsigned, 1);
    assert!(
        channel
            .sent
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("已结束")
    );
}

#[tokio::test]
async fn openid_expired_alerts_once_per_day() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    // 2026-10-24 实测：失效 = HTTP 401 + 特征串，fixture 按真实行为返回
    *state.expired.lock().unwrap() = true;
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-stale",
        Some(channel.clone()),
        false,
        None,
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(!report.openid_ok);
    assert!(report.notified); // 当日首次告警
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("openid 已失效"));
        assert!(sent[0].contains("teachermate bind"));
    }
    assert!(
        store
            .document("teachermate_alert", &format!("openid:{}", today()))
            .unwrap()
            .is_some()
    );

    // 当日第二轮：告警去重，不再推送
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.openid_ok);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn auto_sign_rejected_never_retries() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    *state.sign_response.lock().unwrap() = "你不在我附近".into();
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        "openid-test-0005",
        Some(channel.clone()),
        true,
        None,
    );

    // 首轮：尝试被平台拒绝 → ❌ 回执 + 打「已尝试」标记
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("自动签到被平台拒绝"));
        assert!(sent[0].contains("你不在我附近"));
    }

    // 下轮活动仍待签：不重复尝试（sign 命中数不变），也不重复推送
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    assert!(!report.notified);
    assert_eq!(*state.sign_hits.lock().unwrap(), 2);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sign_manual_signs_and_marks_snapshot() {
    let state = Arc::new(FixtureState::new(signs_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-test-0006",
        Some(channel.clone()),
        false,
        Some((30.5, 114.3)),
    );

    // GPS 活动：命令行未给坐标 → 用配置缺省
    let response = tracker.sign_manual(2002, None, None).await.unwrap();
    assert_eq!(response, r#"{"signRank":2,"studentRank":7}"#);
    {
        let body = state.last_sign_body.lock().unwrap();
        assert_eq!(body["courseId"], 202);
        assert_eq!(body["signId"], 2002);
        assert_eq!(body["lat"], 30.5);
        assert_eq!(body["lon"], 114.3);
    }
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("✅"));
        assert!(sent[0].contains("高等数学"));
    }
    // 手动签已落快照 signed_at → poll 不再补结束回执；
    // 101#1001 从未被 poll 见过（无快照），结束收敛也不会虚构记录
    *state.signs.lock().unwrap() = signs_empty_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.ended_unsigned, 0);

    // 未知活动 ID → 明确报错，不发请求也不推送
    let err = tracker.sign_manual(9999, None, None).await.unwrap_err();
    assert!(err.to_string().contains("找不到进行中的签到 9999"));
    assert_eq!(*state.sign_hits.lock().unwrap(), 1);

    // 二维码活动 → 拒绝代签
    *state.signs.lock().unwrap() = signs_qr_json();
    let err = tracker.sign_manual(3003, None, None).await.unwrap_err();
    assert!(err.to_string().contains("无法代签"));
}

#[tokio::test]
async fn bind_writes_openid_and_verifies_role() {
    let state = Arc::new(FixtureState::new(signs_empty_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let openid_path = dir.path().join("teachermate").join("openid");

    let link = "https://v18.teachermate.cn/wechat-pro-ssr/student/sign?openid=abcdef0123456789abcdef0123456789";
    let outcome = ru41::modules::teachermate::cli::bind_json_with_base(
        link,
        &openid_path,
        &format!("http://{addr}"),
    )
    .await
    .unwrap();
    assert_eq!(outcome["valid"], true);
    assert_eq!(outcome["openid"], "abcdef0123456789abcdef0123456789");
    assert!(outcome["student"].as_str().unwrap().contains("某某大学"));
    // openid 已落盘
    assert_eq!(
        std::fs::read_to_string(&openid_path).unwrap(),
        "abcdef0123456789abcdef0123456789"
    );

    // openid 失效 → bind 报错（文件仍已落盘，便于对比排查）
    *state.role_ok.lock().unwrap() = false;
    let err = ru41::modules::teachermate::cli::bind_json_with_base(
        "fedcba9876543210fedcba9876543210",
        &openid_path,
        &format!("http://{addr}"),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("登录信息失效"));

    // 无法提取 openid 的输入 → 明确报错
    let err = ru41::modules::teachermate::cli::bind_json_with_base(
        "https://example.com/no-openid",
        &openid_path,
        &format!("http://{addr}"),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("无法从输入提取 openid"));
}

#[test]
fn section_parses_minimal_toml_and_resolves_openid_file() {
    let section: TeachermateSection = toml::from_str("").unwrap();
    assert!(!section.is_ready());

    let dir = tempfile::tempdir().unwrap();
    let openid_path = dir.path().join("openid");
    std::fs::write(&openid_path, "abcdef0123456789abcdef0123456789").unwrap();
    let section: TeachermateSection = toml::from_str(&format!(
        "openid_path = {:?}\nnotify = false\ndefault_lat = 30.5\ndefault_lon = 114.3",
        openid_path.display().to_string()
    ))
    .unwrap();
    assert!(section.is_ready());
    let config = section.resolved("target".into()).unwrap();
    assert_eq!(config.openid, "abcdef0123456789abcdef0123456789");
    assert_eq!(config.notify_target, "target");
    assert_eq!(
        config.interval_secs,
        TeachermateSection::DEFAULT_INTERVAL_SECS
    );
    assert!(!config.auto_sign);
    assert_eq!(config.default_location, Some((30.5, 114.3)));

    let err = toml::from_str::<TeachermateSection>("unknown_field = 1").unwrap_err();
    assert!(err.to_string().contains("unknown_field"));
}

#[test]
fn resolved_requires_openid() {
    // 环境变量未设、无配置、openid_path 不存在 → 明确报错
    let section: TeachermateSection = toml::from_str("notify = true").unwrap();
    let err = section.resolved(String::new()).unwrap_err();
    assert!(err.to_string().contains("openid required"));
}

fn make_work_tracker(
    store: Arc<dyn TaskStore>,
    base: &str,
    openid: &str,
    notifier: Option<Arc<dyn NotifyChannel>>,
    todo_sync: bool,
) -> TeachermateWorkTracker {
    let client = TeachermateClient::with_api_base(openid, base.to_owned());
    TeachermateWorkTracker::new(
        store,
        client,
        notifier,
        TeachermateConfig {
            openid: openid.into(),
            openid_path: std::path::PathBuf::from("unused-openid"),
            interval_secs: 60,
            notify_target: "user:mock".into(),
            auto_sign: false,
            default_location: None,
            work_poll_secs: 1800,
            work_due_days: 3,
            work_todo_sync: todo_sync,
            work_vault_sync: false,
            vault_course_dir: None,
            vault_courses: Default::default(),
            work_vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
        },
        ru41::domain::todo::TodoOptions::default(),
        None,
    )
}

/// 相对当前时刻的截止时间串（平台形态 "YYYY-MM-DD HH:MM"；宽容解析按北京时间）。
fn deadline_in(days: i64) -> String {
    use chrono::{Datelike, Timelike};
    let dt = chrono::Utc::now() + chrono::Duration::days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute()
    )
}

#[tokio::test]
async fn work_poll_notifies_new_open_then_daily_dedup() {
    let state = Arc::new(FixtureState::new(signs_empty_json()));
    // 平台实测信封：{"courses":[...]}（2026-10-24 真实响应形态）
    *state.hw_courses.lock().unwrap() =
        json!({"courses": [{"id": 101, "name": "大学英语"}]}).to_string();
    *state.hw_lists.lock().unwrap() = std::collections::BTreeMap::from([(
        101,
        json!({
            "homeworkList": [
                {"id": 5001, "name": "Unit 5 作文", "status": 1,
                 "deadline": deadline_in(1), "submitTime": null}
            ],
            "assessments": []
        })
        .to_string(),
    )]);
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_work_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-work-0001",
        Some(channel.clone()),
        false,
    );

    // 首轮：新未完成作业 → 📝（临期 ⏰ 从下一轮的日去重窗口进入，同 chaoxing 语义）
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.courses, 1);
    assert_eq!(report.works, 1);
    assert_eq!(report.new_opens, 1);
    assert_eq!(report.due_soon, 0);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("【微助教作业】"));
        assert!(sent[0].contains("📝大学英语｜Unit 5 作文｜未完成"));
    }
    let snap = store
        .document("teachermate_work", "101#5001")
        .unwrap()
        .unwrap();
    assert_eq!(snap.value["status"], 1);
    assert_eq!(snap.value["course"], "大学英语");

    // 第二轮：进入临期窗口 → ⏰ 每日一次
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_opens, 0);
    assert_eq!(report.due_soon, 1);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("⏰大学英语"));
    }

    // 当日第三轮：新作业与临期提醒都已去重，不再推送
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_opens, 0);
    assert_eq!(report.due_soon, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn work_status_transitions_and_todo_lifecycle() {
    let state = Arc::new(FixtureState::new(signs_empty_json()));
    *state.hw_courses.lock().unwrap() = json!([{"id": 202, "name": "高等数学"}]).to_string();
    let set_status = |state: &Arc<FixtureState>, status: i64, submit: Option<&str>| {
        *state.hw_lists.lock().unwrap() = std::collections::BTreeMap::from([(
            202,
            json!({
                "homeworkList": [
                    {"id": 6001, "name": "第三章习题", "status": status,
                     "deadline": deadline_in(2), "submitTime": submit}
                ],
                "assessments": []
            })
            .to_string(),
        )]);
    };
    set_status(&state, 1, None);
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_work_tracker(
        store.clone(),
        &format!("http://{addr}"),
        "openid-work-0002",
        Some(channel.clone()),
        true, // todo 同步开
    );

    // 首轮：创建待办
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.todo_created, 1);
    let todo_id = store
        .document("teachermate_work", "202#6001")
        .unwrap()
        .unwrap()
        .value["todo_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!todo_id.is_empty());

    // 1 → 4 已提交：✅ 回执 + 待办回勾
    set_status(&state, 4, Some("2026-10-24 12:00"));
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.submitted, 1);
    {
        let sent = channel.sent.lock().unwrap();
        assert!(
            sent.last()
                .unwrap()
                .contains("✅高等数学｜第三章习题｜已提交")
        );
    }

    // 4 → 2 被打回：❗ 需重交警告
    set_status(&state, 2, None);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.status_changes, 1);
    {
        let sent = channel.sent.lock().unwrap();
        assert!(
            sent.last()
                .unwrap()
                .contains("❗高等数学｜第三章习题｜作业被打回")
        );
    }

    // 2 → 7 补交窗口：📝 提示
    set_status(&state, 7, None);
    let report = tracker.poll_once().await.unwrap();
    {
        let sent = channel.sent.lock().unwrap();
        assert!(sent.last().unwrap().contains("老师已允许补交"));
    }
    let _ = report;

    // 快照收敛：状态数字与提交时间落库
    let snap = store
        .document("teachermate_work", "202#6001")
        .unwrap()
        .unwrap();
    assert_eq!(snap.value["status"], 7);
}

#[tokio::test]
async fn work_overdue_alerts_once_per_day() {
    let state = Arc::new(FixtureState::new(signs_empty_json()));
    *state.hw_courses.lock().unwrap() = json!([{"id": 303, "name": "线性代数"}]).to_string();
    *state.hw_lists.lock().unwrap() = std::collections::BTreeMap::from([(
        303,
        json!({
            "homeworkList": [
                {"id": 7001, "name": "矩阵作业", "status": 1,
                 "deadline": deadline_in(-1), "submitTime": null}
            ],
            "assessments": []
        })
        .to_string(),
    )]);
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_work_tracker(
        store,
        &format!("http://{addr}"),
        "openid-work-0003",
        Some(channel.clone()),
        false,
    );

    // 首轮：新未完成 → 📝；第二轮进入过期窗口 → ⚠️ 每日一次
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_opens, 1);
    assert_eq!(report.overdue, 0);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.overdue, 1);
    {
        let sent = channel.sent.lock().unwrap();
        assert!(sent[0].contains("📝线性代数"));
        assert!(sent[1].contains("⚠️线性代数｜矩阵作业｜已过期未交"));
    }
    // 当日第三轮去重
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.overdue, 0);
    assert!(!report.notified);
}

fn today() -> String {
    crate_today()
}

fn crate_today() -> String {
    use chrono::Datelike;
    let dt = chrono::Utc::now();
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}
