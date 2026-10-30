use std::{
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{RawQuery, State},
    response::AppendHeaders,
    routing::{get, post},
};
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{
        NotifyRoute,
        todo::{TodoItem, TodoStatus},
    },
    modules::chaoxing::{
        ChaoxingClient, ChaoxingConfig, ChaoxingSection, ChaoxingTracker, sign_type_name,
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

/// fixture 服务状态：登录/活动列表/签到响应可控（模拟登录失效、老师结束签到、
/// 签到失败），并记录 preSign 与签到命中参数。
/// work_list_* 字段由 fixture 服务写入、供断言路径间接使用，这里整体允许未读。
#[derive(Default)]
#[allow(dead_code)]
struct FixtureState {
    /// 平台登录是否放行；Default 手写为 true，失败场景显式置 false。
    login_ok: Mutex<bool>,
    activities: Mutex<String>,
    presign_hits: Mutex<usize>,
    /// stuSignajax（实际签到请求）命中次数。
    sign_hits: Mutex<usize>,
    /// stuSignajax 最后一次请求的 (activeId, latitude)。
    last_sign: Mutex<Option<(String, String)>>,
    /// stuSignajax 最后一次完整 query（enc/objectId/address 断言用）。
    last_query: Mutex<String>,
    /// 云盘 /upload 命中次数（拍照签到）。
    upload_hits: Mutex<usize>,
    /// 作业列表 HTML（详情直链需含 fixture 自身地址，spawn 后注入）。
    work_list_html: Mutex<String>,
    /// 作业详情页 HTML。
    work_detail_html: Mutex<String>,
    /// /mooc2/work/list 命中次数。
    work_list_hits: Mutex<usize>,
    /// stuSignajax 返回体（默认 success，可注入失败文本）。
    sign_response: Mutex<String>,
}

impl FixtureState {
    fn new(activities: String) -> Self {
        Self {
            login_ok: Mutex::new(true),
            activities: Mutex::new(activities),
            presign_hits: Mutex::new(0),
            sign_hits: Mutex::new(0),
            last_sign: Mutex::new(None),
            last_query: Mutex::new(String::new()),
            upload_hits: Mutex::new(0),
            work_list_html: Mutex::new(String::new()),
            work_detail_html: Mutex::new(String::new()),
            work_list_hits: Mutex::new(0),
            sign_response: Mutex::new("success".into()),
        }
    }
}

const COURSE_HTML: &str = r#"<div class="course clearfix  learnCourse stu_157361070" info="157361070_593022622" roleId="stu_157361070" id="c_268129408" >
    <div class="course-cover">
        <input type="hidden" class="clazzId" name="clazzId" value="157361070"/>
        <input type="hidden" class="courseId" name="courseId" value="268129408"/>
        <a href="https://mooc1.chaoxing.com/visit/stucoursemiddle?courseid=268129408&clazzid=157361070&cpi=593022622&ismooc2=1&v=2">
        <span class="course-name overHidden2" style="word-break: break-all;" title="大学英语新视野">大学英语新视野</span>
        <p class="line2 color3" title="王金妮">王金妮</p>
    </div>
</div>
<div class="course clearfix notOpen" id="c_8" >
    <a class="not-open-tip"></a>
    <input type="hidden" class="clazzId" name="clazzId" value="9"/>
    <input type="hidden" class="courseId" name="courseId" value="8"/>
    <a href="https://mooc1.chaoxing.com/visit/stucoursemiddle?courseid=8&clazzid=9&cpi=7&ismooc2=1&v=2">
    <span class="course-name overHidden2" title="未开放课">未开放课</span>
    </a>
</div>"#;

fn activities_pending_json() -> String {
    json!({
        "result": 1,
        "data": {"activeList": [
            {"id": 1001, "activeType": 2, "type": 0, "nameOne": "普通签到",
             "status": 1, "userStatus": 1, "attendNum": 3},
            {"id": 1002, "activeType": 2, "type": 2, "nameOne": "二维码签到",
             "status": 2, "userStatus": 1, "attendNum": 50},
            {"id": 1003, "activeType": 11, "type": 11, "nameOne": "选人",
             "status": 1, "userStatus": 1}
        ]}
    })
    .to_string()
}

fn activities_resolved_json() -> String {
    json!({
        "result": 1,
        "data": {"activeList": [
            {"id": 1001, "activeType": 2, "type": 0, "nameOne": "普通签到",
             "status": 2, "userStatus": 0, "attendNum": 57}
        ]}
    })
    .to_string()
}

async fn spawn_fixture(state: Arc<FixtureState>) -> SocketAddr {
    let app = Router::new()
        .route(
            "/fanyalogin",
            post(|State(state): State<Arc<FixtureState>>| async move {
                let status = *state.login_ok.lock().unwrap();
                let body =
                    Json(json!({"status": status, "msg2": if status { "" } else { "验证码错误" }}));
                if status {
                    // 多个 set-cookie 必须用 AppendHeaders，数组形式会同名覆盖
                    (
                        AppendHeaders([
                            ("set-cookie", "_uid=487480814; Path=/"),
                            ("set-cookie", "fid=1024; Path=/"),
                        ]),
                        body,
                    )
                } else {
                    (
                        AppendHeaders([("cache-control", "no-store"), ("x-login", "failed")]),
                        body,
                    )
                }
            }),
        )
        .route(
            "/mooc/accountManage",
            get(|| async { r#"<input id="messageName" value="蔡承峰">"# }),
        )
        .route(
            "/mooc2-ans/visit/courselistdata",
            post(|| async { COURSE_HTML }),
        )
        .route(
            "/v2/apis/active/student/activelist",
            get(|State(state): State<Arc<FixtureState>>| async move {
                state.activities.lock().unwrap().clone()
            }),
        )
        .route(
            "/newsign/preSign",
            get(|State(state): State<Arc<FixtureState>>| async move {
                *state.presign_hits.lock().unwrap() += 1;
                "ok"
            }),
        )
        .route(
            "/pptSign/stuSignajax",
            get(
                |State(state): State<Arc<FixtureState>>, RawQuery(raw): RawQuery| async move {
                    let query = raw.unwrap_or_default();
                    let get = |key: &str| {
                        query.split('&').find_map(|pair| {
                            let (k, v) = pair.split_once('=')?;
                            (k == key).then(|| v.to_string())
                        })
                    };
                    *state.last_sign.lock().unwrap() = Some((
                        get("activeId").unwrap_or_default(),
                        get("latitude").unwrap_or_default(),
                    ));
                    *state.last_query.lock().unwrap() = query.clone();
                    *state.sign_hits.lock().unwrap() += 1;
                    state.sign_response.lock().unwrap().clone()
                },
            ),
        )
        .route(
            "/api/token/uservalid",
            get(|| async { Json(json!({"_token": "test-token"})) }),
        )
        .route(
            "/upload",
            post(
                |State(state): State<Arc<FixtureState>>, body: axum::body::Bytes| async move {
                    *state.upload_hits.lock().unwrap() += 1;
                    // multipart 里必须带 puid 表单域
                    let text = String::from_utf8_lossy(&body);
                    assert!(text.contains("puid"));
                    Json(json!({"objectId": "pan-object-1"}))
                },
            ),
        )
        // 作业链路：stucoursemiddle 隐藏域三件套（含 name 属性夹在中间）
        .route(
            "/visit/stucoursemiddle",
            get(|| async {
                r#"<input type="hidden" id="openc" name="openc" value="openc-1"/>
<input type="hidden" id="enc" name="enc" value="pageenc-2"/>
<input type="hidden" id="workEnc" name="workEnc" value="workenc-9"/>"#
            }),
        )
        // 作业列表：enc 必须用 workEnc，否则无权限文案
        .route(
            "/mooc2/work/list",
            get(
                |State(state): State<Arc<FixtureState>>, RawQuery(raw): RawQuery| async move {
                    let query = raw.unwrap_or_default();
                    if !query.contains("enc=workenc-9") || !query.contains("stuenc=pageenc-2") {
                        return "您长时间没有操作，或没有此页面访问权限，请尝试重新进入课程[3004]"
                            .to_string();
                    }
                    *state.work_list_hits.lock().unwrap() += 1;
                    state.work_list_html.lock().unwrap().clone()
                },
            ),
        )
        .route(
            "/mooc-ans/mooc2/work/task",
            get(|State(state): State<Arc<FixtureState>>| async move {
                state.work_detail_html.lock().unwrap().clone()
            }),
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
    cookie_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
) -> ChaoxingTracker {
    make_tracker_with(store, base, cookie_path, notifier, None, None)
}

fn make_tracker_with(
    store: Arc<dyn TaskStore>,
    base: &str,
    cookie_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    default_location: Option<(f64, f64, String)>,
    photo_path: Option<std::path::PathBuf>,
) -> ChaoxingTracker {
    make_tracker_full(
        store,
        base,
        cookie_path,
        notifier,
        default_location,
        photo_path,
        false,
    )
}

fn make_tracker_full(
    store: Arc<dyn TaskStore>,
    base: &str,
    cookie_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    default_location: Option<(f64, f64, String)>,
    photo_path: Option<std::path::PathBuf>,
    auto_sign: bool,
) -> ChaoxingTracker {
    let client = ChaoxingClient::with_base_urls(
        "10000000000",
        "pw",
        cookie_path.clone(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
    );
    ChaoxingTracker::new(
        store,
        client,
        notifier,
        ChaoxingConfig {
            phone: "10000000000".into(),
            password: "pw".into(),
            cookie_path,
            interval_secs: 60,
            notify_target: "user:mock".into(),
            auto_sign,
            default_location,
            photo_path,
            work_poll_secs: 1800,
            work_due_days: 3,
            work_todo_sync: true,
            work_vault_sync: true,
            vault_course_dir: None,
            vault_courses: Default::default(),
            work_vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
            enc_sample_path: None,
        },
    )
}

/// 插一条覆盖当前时刻的 jwxt 课表事件：课表窗口轮询默认只在学校时段实轮询，
/// 测试需要窗口放行才能走到活动枚举。
fn insert_covering_class_event(store: &SqliteTaskStore) {
    let now = ru41::util::now();
    store
        .put_document(
            "event",
            "evt_test_window",
            None,
            &serde_json::json!({
                "id": "evt_test_window",
                "title": "大学英语新视野",
                "status": "active",
                "starts_at": now - 600,
                "ends_at": now + 3600,
                "location": null,
                "recurrence": "none",
                "reminded_starts": [],
                "created_at": now,
                "updated_at": now,
                "source": {"channel": "jwxt", "source_id": "test"},
            }),
        )
        .unwrap();
}

#[tokio::test]
async fn notifies_pending_once_then_idempotent() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    insert_covering_class_event(&store);
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.courses, 1); // 未开放课被跳过
    assert_eq!(report.sign_activities, 2); // 选人活动不计入
    assert_eq!(report.new_activities, 2);
    assert_eq!(report.pending, 1);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("普通签到"));
        assert!(sent[0].contains("chaoxing sign 1001"));
    }
    assert_eq!(
        store
            .document("chaoxing_sign", "268129408#1001")
            .unwrap()
            .map(|d| d
                .value
                .get("course")
                .and_then(|v| v.as_str())
                .map(str::to_owned)),
        Some(Some("大学英语新视野".into()))
    );

    // 第二轮：活动仍待签但 records 已见 → 不重复提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_activities, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);

    // 第三阶段：活动结束且已签（(1,1)→(2,0)）→ 状态变迁回执 ✅
    *state.activities.lock().unwrap() = activities_resolved_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.pending, 0);
    assert_eq!(report.signed, 1);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("签到回执"));
        assert!(sent[1].contains("签到成功"));
    }

    // 第四轮：状态无变化 → 不再重复播报
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.signed, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn missed_sign_transition_notifies_once() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    insert_covering_class_event(&store);
    let channel = mock_channel();
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
    );
    let report = tracker.poll_once().await.unwrap();
    assert!(report.notified);

    // 老师结束签到且本人未签：(1,1)→(2,1) → ❌ 漏签回执
    *state.activities.lock().unwrap() = json!({
        "result": 1,
        "data": {"activeList": [
            {"id": 1001, "activeType": 2, "type": 0, "nameOne": "普通签到",
             "status": 2, "userStatus": 1}
        ]}
    })
    .to_string();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.missed, 1);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("仍未签到"));
    }

    // 再轮询无重复
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn login_failure_alerts_once_per_day() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    *state.login_ok.lock().unwrap() = false;
    let addr = spawn_fixture(state).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    insert_covering_class_event(&store);
    let channel = mock_channel();
    let tracker = make_tracker(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert_eq!(report.courses, 0);
    assert!(report.notified); // 当日首次告警
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("登录态失效"));
    }
    assert!(
        store
            .document("chaoxing_alert", &format!("login:{}", today()))
            .unwrap()
            .is_some()
    );

    // 当日第二轮：告警去重，不再推送
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.login_ok);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sign_manual_signs_via_presign_and_reports() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    insert_covering_class_event(&store);
    let channel = mock_channel();
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
    );

    let response = tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(response, "success");
    assert_eq!(*state.presign_hits.lock().unwrap(), 1);
    let last = state.last_sign.lock().unwrap().clone().unwrap();
    assert_eq!(last.0, "1001");
    assert_eq!(last.1, "-1"); // 普通签到不带位置
    {
        // 签到成功 → 推送宿主
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("签到成功"));
        assert!(sent[0].contains("大学英语新视野"));
    }

    // 平台随后收敛为已签结束：poll 不把命令行签到再播一次
    *state.activities.lock().unwrap() = activities_resolved_json();
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);

    // 未知活动 ID → 明确报错，不发请求也不推送
    let err = tracker
        .sign_manual("9999", None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("找不到签到活动 9999"));
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sign_failure_pushes_failure_receipt() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    *state.sign_response.lock().unwrap() = "距离太远".into();
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    insert_covering_class_event(&store);
    let channel = mock_channel();
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
    );

    // 平台返回失败文本：命令行仍拿到原文，但推送 ❌ 回执
    let response = tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(response, "距离太远");
    let sent = channel.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("签到失败"));
    assert!(sent[0].contains("距离太远"));
}

/// 造一个指定子类型的进行中未签活动（id 固定 1001）。
fn pending_of_type(sign_type: i64, label: &str) -> String {
    json!({
        "result": 1,
        "data": {"activeList": [
            {"id": 1001, "activeType": 2, "type": sign_type, "nameOne": label,
             "status": 1, "userStatus": 1}
        ]}
    })
    .to_string()
}

#[tokio::test]
async fn photo_sign_uploads_then_signs_with_object_id() {
    let state = Arc::new(FixtureState::new(pending_of_type(1, "拍照签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let photo = dir.path().join("me.jpg");
    std::fs::write(&photo, b"jpeg-bytes").unwrap();
    let tracker = make_tracker_with(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
        Some(photo),
    );

    let response = tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(response, "success");
    assert_eq!(*state.upload_hits.lock().unwrap(), 1);
    // 拍照不走 preSign（对齐参考实现）
    assert_eq!(*state.presign_hits.lock().unwrap(), 0);
    let query = state.last_query.lock().unwrap().clone();
    assert!(query.contains("objectId=pan-object-1"));
    assert!(!query.contains("objectId=aaa"));
    assert!(query.contains("latitude=-1"));
    assert!(channel.sent.lock().unwrap()[0].contains("拍照签到"));
}

#[tokio::test]
async fn photo_sign_falls_back_to_builtin_photo() {
    let state = Arc::new(FixtureState::new(pending_of_type(1, "拍照签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let cookie_path = dir.path().join("cookies.json");
    let tracker = make_tracker(
        store,
        &format!("http://{addr}"),
        cookie_path.clone(),
        Some(channel),
    );

    // 未配置照片 → 内置兜底照片落盘到 cookie 同目录并上传
    tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(*state.upload_hits.lock().unwrap(), 1);
    let fallback = dir.path().join("fallback-photo.jpg");
    assert!(fallback.is_file());
    assert_eq!(
        std::fs::read(&fallback).unwrap(),
        ru41::modules::chaoxing::fallback_photo_bytes()
    );
}

#[tokio::test]
async fn qr_sign_requires_real_enc_or_photo() {
    let state = Arc::new(FixtureState::new(pending_of_type(2, "二维码签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker_with(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        Some((30.5, 114.3, "宿舍".into())),
        None,
    );

    // 缺 enc → 拒签并指路图片通道（伪造 enc 已被平台证伪：2026-10-23 实测
    // 「签到失败，请重新扫描。」）。enc 只在二维码图片里，自动签必须图片驱动。
    let err = tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("二维码照片发给机器人"));

    // 显式 --enc（来自解出的真实二维码图片）走正常签到
    let response = tracker
        .sign_manual("1001", None, None, None, Some("ENC123"), None)
        .await
        .unwrap();
    assert_eq!(response, "success");
    let query = state.last_query.lock().unwrap().clone();
    assert!(query.contains("enc=ENC123"));
    assert!(query.contains("location="));
    assert!(query.contains("30.5")); // 默认坐标进了 location blob
    assert!(query.contains("latitude=-1")); // 主坐标参数仍为 -1
}

#[tokio::test]
async fn location_sign_prefers_cli_then_config_default() {
    let state = Arc::new(FixtureState::new(pending_of_type(4, "位置签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = make_tracker_with(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        None,
        Some((30.5, 114.3, "宿舍".into())),
        None,
    );

    // 无命令行坐标 → 用配置的默认位置
    tracker
        .sign_manual("1001", None, None, None, None, None)
        .await
        .unwrap();
    let query = state.last_query.lock().unwrap().clone();
    assert!(query.contains("latitude=30.5"));
    assert!(query.contains("longitude=114.3"));
    assert!(query.contains("ifTiJiao=1"));
    assert!(query.contains("address="));

    // 命令行坐标优先于默认配置
    tracker
        .sign_manual("1001", Some(31.0), Some(115.0), Some("教学楼"), None, None)
        .await
        .unwrap();
    let query = state.last_query.lock().unwrap().clone();
    assert!(query.contains("latitude=31"));
    assert!(query.contains("longitude=115"));
    assert!(query.contains("address="));
}

// ---- 自动签到 ----

#[tokio::test]
async fn auto_sign_signs_pending_on_discovery_and_reports() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker_full(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
        None,
        true,
    );

    // 首轮：发现待签 → 当场自动签 → ✅ 自动回执，无 ⏰ 提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 1);
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("自动签到成功"));
        assert!(!sent[0].contains("⏰"));
    }
    assert_eq!(*state.presign_hits.lock().unwrap(), 1);
    let doc = store
        .document("chaoxing_sign", "268129408#1001")
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["user_status"], 0);
    assert_eq!(doc.value["signed_via"], "auto");

    // 平台收敛为已签结束 → 不再播报（auto 的 ✅ 已覆盖）
    *state.activities.lock().unwrap() = activities_resolved_json();
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.signed, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn auto_sign_platform_reject_marks_and_never_retries() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    *state.sign_response.lock().unwrap() = "距离太远".into();
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker_full(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        Some((23.4535, 113.4905, "教学楼".into())),
        None,
        true,
    );

    // 首轮：尝试被平台拒绝 → ❌ 回执 + 打「已尝试」标记
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("自动签到被平台拒绝"));
        assert!(sent[0].contains("距离太远"));
    }

    // 下轮活动仍待签：不重复尝试（sign 命中数不变），也不重复推送
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    assert!(!report.notified);
    assert_eq!(*state.presign_hits.lock().unwrap(), 1);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn auto_sign_skips_qr_and_reminds_photo() {
    let state = Arc::new(FixtureState::new(pending_of_type(2, "二维码签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker_full(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
        None,
        true,
    );

    // 二维码不进自动签（enc 伪造被平台证伪）：推送⏰提醒并指路图片通道，
    // 不发 sign 请求、不打「已尝试」标记（等用户把二维码照片发给机器人）。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.auto_signed, 0);
    assert_eq!(*state.presign_hits.lock().unwrap(), 0);
    assert_eq!(*state.sign_hits.lock().unwrap(), 0);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("二维码签到"));
        assert!(sent[0].contains("把二维码照片发给机器人"));
    }
    let doc = store
        .document("chaoxing_sign", "268129408#1001")
        .unwrap()
        .unwrap();
    assert!(doc.value.get("signed_via").is_none());
    assert!(doc.value.get("auto_attempted").is_none());
}

/// 回归（2026-10-22 真实漏签）：activelist 的 type 字段恒为 2（=签到类），
/// 种类只在 nameOne 文案里。手势签到曾是「type=2 被当二维码跳过」的死代码。
/// 2026-10-30 起手势被平台风控封堵（直签稳定 90002），改为即发现即强提醒。
#[tokio::test]
async fn gesture_sign_alerts_immediately_without_auto_attempt() {
    // 真实形状：type=2 + nameOne=手势签到（今早 07:29 实际漏掉的那个）
    let state = Arc::new(FixtureState::new(pending_of_type(2, "手势签到")));
    let addr = spawn_fixture(state.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_tracker_full(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
        None,
        true,
    );

    // 手势不自动签（90002 确定性失败）：不发 sign 请求、不打「已尝试」标记，
    // 推送 🚨 强提醒指路手动签；下一轮活动仍待签也不补试、不重复推送
    for _ in 0..2 {
        let report = tracker.poll_once().await.unwrap();
        assert_eq!(report.auto_signed, 0);
        assert_eq!(*state.presign_hits.lock().unwrap(), 0);
        assert_eq!(*state.sign_hits.lock().unwrap(), 0);
    }
    let sent = channel.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("手势签到"));
    assert!(sent[0].contains("手动签"));
}

/// 种类识别以 nameOne 文案为准，type 字段不可信。
#[test]
fn sign_kind_is_derived_from_platform_label_not_type_field() {
    use ru41::modules::chaoxing::source::{ChaoxingActivity, ChaoxingSignKind};
    let mk = |sign_type: i64, name_one: &str| ChaoxingActivity {
        id: 1,
        active_type: Some(2),
        sign_type: Some(sign_type),
        name_one: Some(name_one.into()),
        start_time: None,
        end_time: None,
        status: Some(1),
        user_status: Some(1),
        attend_num: None,
    };
    // 关键对：同一 type=2，标签不同 → 种类不同（线上真实数据形状）
    assert_eq!(mk(2, "手势签到").sign_kind(), ChaoxingSignKind::Gesture);
    assert_eq!(mk(2, "二维码签到").sign_kind(), ChaoxingSignKind::Qr);
    assert!(mk(2, "二维码签到").is_qr());
    assert!(!mk(2, "手势签到").is_qr());
    assert_eq!(mk(4, "位置签到").sign_kind(), ChaoxingSignKind::Location);
    assert_eq!(mk(1, "拍照签到").sign_kind(), ChaoxingSignKind::Photo);
    // 标签缺失时退回 type 字段的文档映射
    let mut no_label = mk(3, "");
    no_label.name_one = None;
    assert_eq!(no_label.sign_kind(), ChaoxingSignKind::Gesture);
}

// ---- 作业监控 ----

/// 内存版 Vault（与 zhihuishu 测试同款语义：内容变化才置 dirty，commit 消费 dirty）。
#[derive(Default)]
struct WorkVault {
    files: Mutex<std::collections::BTreeMap<String, String>>,
    dirty: Mutex<bool>,
}

impl ru41::ports::vault::Vault for WorkVault {
    fn fetch(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn remote_head(&self) -> anyhow::Result<String> {
        Ok("0fake".into())
    }
    fn head_subject(&self, _sha: &str) -> anyhow::Result<String> {
        Ok("fake".into())
    }
    fn changed_files(&self, _from: &str, _to: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn changed_entries(&self, _from: &str, _to: &str) -> anyhow::Result<Vec<(String, bool)>> {
        Ok(vec![])
    }
    fn checkout_hard(&self, _sha: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn commit_all(&self, _message: &str) -> anyhow::Result<bool> {
        Ok(std::mem::take(&mut *self.dirty.lock().unwrap()))
    }
    fn push(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn sync(&self) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn read_file(&self, relative: &str) -> anyhow::Result<Option<String>> {
        Ok(self.files.lock().unwrap().get(relative).cloned())
    }
    fn write_file(&self, relative: &str, content: &str) -> anyhow::Result<()> {
        let mut files = self.files.lock().unwrap();
        if files.get(relative).map(String::as_str) != Some(content) {
            *self.dirty.lock().unwrap() = true;
        }
        files.insert(relative.to_owned(), content.to_owned());
        Ok(())
    }
    fn write_bytes(&self, relative: &str, data: &[u8]) -> anyhow::Result<()> {
        self.write_file(relative, &String::from_utf8_lossy(data))
    }
    fn remove_file(&self, relative: &str) -> anyhow::Result<bool> {
        Ok(self.files.lock().unwrap().remove(relative).is_some())
    }
    fn append_inbox_note(
        &self,
        _inbox_dir: &str,
        _text: &str,
        _now: i64,
        _offset_seconds: i32,
    ) -> anyhow::Result<String> {
        Ok("fake-inbox.md".into())
    }
}

/// 作业列表 HTML：一条可配置状态 + 一条已提交，详情直链指向 fixture 自身。
fn work_list_html(addr: &SocketAddr, open_status: &str) -> String {
    let detail = |work_id: u32| {
        format!(
            "http://{addr}/mooc-ans/mooc2/work/task?courseId=268129408&classId=157361070&cpi=593022622&workId={work_id}&answerId=1&enc=abc"
        )
    };
    let item = |work_id: u32, title: &str, status: &str| {
        format!(
            r#"<li data="{}"><div class="tag work-icon icon-zy"></div>
<div class="right-content"><p class="overHidden2 fl">{title}</p><p class="status fl">{status}</p></div></li>"#,
            detail(work_id)
        )
    };
    format!(
        "<ul>{}{}</ul>",
        item(9001, "作业一：函数极限", open_status),
        item(9002, "作业二：导数应用", "已提交")
    )
}

fn work_detail_html(end: &str) -> String {
    format!(
        "<div class=\"mark_name\">作业一</div><p>作答时间: 11-01 08:00 至 {end}</p><span>题量: 5</span><span>满分: 100</span>"
    )
}

fn make_work_tracker(
    store: Arc<dyn TaskStore>,
    base: &str,
    cookie_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    vault: Option<Arc<dyn ru41::ports::vault::Vault>>,
) -> ru41::modules::chaoxing::ChaoxingWorkTracker {
    let client = ChaoxingClient::with_base_urls(
        "10000000000",
        "pw",
        cookie_path.clone(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
        base.to_owned(),
    );
    ru41::modules::chaoxing::ChaoxingWorkTracker::new(
        store,
        client,
        notifier,
        ChaoxingConfig {
            phone: "10000000000".into(),
            password: "pw".into(),
            cookie_path,
            interval_secs: 60,
            notify_target: "user:mock".into(),
            auto_sign: false,
            default_location: None,
            photo_path: None,
            work_poll_secs: 1800,
            work_due_days: 3,
            work_todo_sync: true,
            work_vault_sync: true,
            vault_course_dir: Some("学期".into()),
            vault_courses: Default::default(),
            work_vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
            enc_sample_path: None,
        },
        ru41::domain::todo::TodoOptions {
            offset_seconds: 28_800,
            recent_days: 7,
            long_term_days: 30,
        },
        vault,
    )
}

/// 注入作业 fixture 数据（列表 HTML 需要 fixture 地址）。
fn seed_works(state: &FixtureState, addr: &SocketAddr, open_status: &str, end: &str) {
    *state.work_list_html.lock().unwrap() = work_list_html(addr, open_status);
    *state.work_detail_html.lock().unwrap() = work_detail_html(end);
}

/// 今天 + days 天的日期（YYYY.MM.DD）：vault 笔记名由截止日期生成，断言必须同时钟。
fn due_date_compact(days: i64) -> String {
    (chrono::Local::now() + chrono::Duration::days(days))
        .format("%Y.%m.%d")
        .to_string()
}

/// 今天 + days 天的截止时刻（MM-DD 23:59）：截止日期必须跟随时钟，硬编码日期会让测试随日历腐烂。
fn due_in_days(days: i64) -> String {
    let now = chrono::Local::now() + chrono::Duration::days(days);
    now.format("%m-%d 23:59").to_string()
}

#[tokio::test]
async fn work_poll_discovers_open_and_syncs_todo_and_vault() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    seed_works(&state, &addr, "未交", &due_in_days(5));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let vault = Arc::new(WorkVault::default());
    let tracker = make_work_tracker(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        Some(vault.clone()),
    );

    let report = tracker.poll_once().await.unwrap();
    assert!(report.login_ok);
    assert_eq!(report.works, 2);
    assert_eq!(report.new_opens, 1);
    assert_eq!(report.todo_created, 1);
    assert_eq!(report.vault_files, 4); // 课程索引 + 2 条作业说明 + 作业总表
    assert!(report.notified);
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("📝") && sent[0].contains("作业一：函数极限"));
        // 同步成果不进推送（噪音），只进报告字段
        assert!(!sent[0].contains("已同步 4 个笔记到 Obsidian"));
    }

    // 快照与待办联动
    let snap = store
        .document("chaoxing_work", "268129408#9001")
        .unwrap()
        .unwrap();
    assert_eq!(snap.value["status"], "未交");
    let todo_id = snap.value["todo_id"].as_str().unwrap().to_owned();
    let todo_doc = store.document("todo_item", &todo_id).unwrap().unwrap();
    let item: TodoItem = serde_json::from_value(todo_doc.value).unwrap();
    assert_eq!(item.source.unwrap().channel, "chaoxing");
    assert!(item.due_at.is_some());

    // vault 文件：课程索引、作业说明、跨平台作业总表（锁守卫必须在本作用域释放）
    {
        let files = vault.files.lock().unwrap();
        let note = files.get("学期/大学英语新视野/学习通作业.md").unwrap();
        assert!(note.contains("## 未完成（1）"));
        assert!(note.contains(&format!(
            "[[{}_作业一：函数极限/作业说明|",
            due_date_compact(5)
        )));
        assert!(
            files.contains_key(
                format!(
                    "学期/大学英语新视野/{}_作业一：函数极限/作业说明.md",
                    due_date_compact(5)
                )
                .as_str()
            )
        );
        let dash = files.get("学期/作业总表.md").unwrap();
        assert!(dash.contains("学习通｜大学英语新视野｜作业一：函数极限"));
    }

    // 第二轮：同状态不再提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_opens, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn work_due_soon_nags_once_daily() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    // 今天 +2 天截止 → 进入临期窗口（due_days=3）
    seed_works(&state, &addr, "未交", &due_in_days(2));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_work_tracker(
        store,
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
    );

    // 首轮只播新发现，不播临期（临期从次轮起）
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_opens, 1);
    assert_eq!(report.due_soon, 0);

    // 次轮：临期提醒一次
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.due_soon, 1);
    assert!(channel.sent.lock().unwrap().last().unwrap().contains("⏰"));

    // 三轮：当日去重，不再重复提醒
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.due_soon, 0);
    assert!(!report.notified);
}

#[tokio::test]
async fn work_submitted_transition_completes_todo() {
    let state = Arc::new(FixtureState::new(activities_pending_json()));
    let addr = spawn_fixture(state.clone()).await;
    seed_works(&state, &addr, "未交", &due_in_days(5));
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let channel = mock_channel();
    let tracker = make_work_tracker(
        store.clone(),
        &format!("http://{addr}"),
        dir.path().join("cookies.json"),
        Some(channel.clone()),
        None,
    );

    tracker.poll_once().await.unwrap();
    let todo_id = {
        let snap = store
            .document("chaoxing_work", "268129408#9001")
            .unwrap()
            .unwrap();
        snap.value["todo_id"].as_str().unwrap().to_owned()
    };

    // 平台侧已交：快照更新 + 待办回勾 + ✅ 回执
    seed_works(&state, &addr, "已提交", &due_in_days(5));
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.submitted, 1);
    assert!(channel.sent.lock().unwrap().last().unwrap().contains("✅"));
    let todo_doc = store.document("todo_item", &todo_id).unwrap().unwrap();
    let item: TodoItem = serde_json::from_value(todo_doc.value).unwrap();
    assert_eq!(item.status, TodoStatus::Completed);

    // 再来一轮：无重复回勾
    tracker.poll_once().await.unwrap();
    let n = channel
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.contains("已提交"))
        .count();
    assert_eq!(n, 1);
}

#[test]
fn activity_json_parses_probe_shape() {
    let raw = r#"{"userStatus":1,"nameTwo":"","otherId":"2","groupId":2,"source":15,
        "isLook":1,"type":2,"releaseNum":0,"attendNum":57,"activeType":2,
        "nameOne":"二维码签到","flagLogo":false,"startTime":1792135761000,
        "id":1000174332257,"endTime":1792135881000,"status":2,"nameFour":"10-16 15:31"}"#;
    let a: ru41::modules::chaoxing::ChaoxingActivity = serde_json::from_str(raw).unwrap();
    assert_eq!(a.id, 1000174332257);
    assert!(a.is_sign());
    assert!(!a.is_pending());
    assert_eq!(a.type_label(), "二维码签到");
}

#[test]
fn activity_pending_detection() {
    let mk = |v: serde_json::Value| -> ru41::modules::chaoxing::ChaoxingActivity {
        serde_json::from_value(v).unwrap()
    };
    assert!(
        mk(json!({"id":1,"activeType":2,"type":0,"nameOne":"普通签到","status":1,"userStatus":1}))
            .is_pending()
    );
    assert!(!mk(json!({"id":2,"activeType":2,"status":1,"userStatus":0})).is_pending());
    let picker =
        mk(json!({"id":3,"activeType":11,"type":11,"nameOne":"选人","status":1,"userStatus":1}));
    assert!(!picker.is_sign());
    assert!(!picker.is_pending());
}

#[test]
fn sign_type_names_cover_known_types() {
    assert_eq!(sign_type_name(Some(0)), "普通签到");
    assert_eq!(sign_type_name(Some(2)), "二维码签到");
    assert_eq!(sign_type_name(Some(4)), "位置签到");
    assert_eq!(sign_type_name(Some(99)), "签到");
    assert_eq!(sign_type_name(None), "签到");
}

#[test]
fn section_parses_minimal_toml_and_rejects_unknown() {
    let section: ChaoxingSection = toml::from_str("").unwrap();
    assert!(section.phone.is_none());
    assert!(!section.is_ready());

    let section: ChaoxingSection =
        toml::from_str("phone = \"10000000000\"\npassword = \"pw\"\nnotify = false").unwrap();
    assert!(section.is_ready());
    let config = section.resolved("target".into()).unwrap();
    assert_eq!(config.notify_target, "target");
    assert_eq!(config.interval_secs, ChaoxingSection::DEFAULT_INTERVAL_SECS);

    let err = toml::from_str::<ChaoxingSection>("unknown_field = 1").unwrap_err();
    assert!(err.to_string().contains("unknown_field"));
}

#[test]
fn resolved_requires_credentials() {
    let section: ChaoxingSection = toml::from_str("notify = true").unwrap();
    let err = section.resolved(String::new()).unwrap_err();
    assert!(err.to_string().contains("phone required"));
}

#[test]
fn notify_route_shape_matches_dispatcher() {
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "open_kid".into(),
        msg_id: None,
    };
    assert_eq!(route.channel, "qq");
}

fn today() -> String {
    use chrono::Datelike;
    let dt = chrono::Utc::now();
    format!("{:04}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

/// 钉住 DEF.image_consumers 接线：合并/重构最容易把它静默扫成 noop
/// （bfc2a69 曾把照片签到断线而测试全绿）。配置齐 → 恰好 1 个消费者。
#[test]
fn def_image_consumers_wires_qr_signer_when_ready() {
    let config: ru41::bootstrap::config::Config = toml::from_str(
        r#"
[sources.chaoxing]
phone = "10000000000"
password = "pw"
"#,
    )
    .unwrap();
    let consumers = ru41::bootstrap::assembly::qq_image_consumers(&config).unwrap();
    assert_eq!(
        consumers.len(),
        1,
        "[sources.chaoxing] 配置齐时必须接上二维码照片签到消费者"
    );
}
