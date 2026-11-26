//! 校园跑附属的集成测试：拿一个本地 mock 控制台验证
//! 请求路径、令牌头、错误映射与渲染结果。
//!
//! 真机形态：pace-forge 控制台在 9999 端口，`/api/*` 要 `X-Pace-Token`，
//! 返回 `{"ok":true,"data":{...}}`。

use axum::{Router, routing::get};
use ru41_plugin_campusrun::{CampusrunClient, CampusrunSection};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 起一个 mock 控制台：按路径返回固定数据，并记录收到的令牌头。
async fn mock_console() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = seen.clone();
    let app = Router::new()
        .route(
            "/api/semester",
            get(move |headers: axum::http::HeaderMap| {
                let recorder = recorder.clone();
                async move {
                    recorder.lock().await.push(
                        headers
                            .get("x-pace-token")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned(),
                    );
                    axum::Json(json!({
                        "ok": true,
                        "data": {
                            "summaryRaw": {
                                "semesterDis": 7511,
                                "semesterValidDis": 6295,
                                "sname": "2026-2027-1体能训练Ⅰ",
                                "semesterCount": 0,
                                "semesterValidCount": 0
                            },
                            "goalRaw": {
                                "sid": 9828,
                                "startDate": 1793376000000i64,
                                "endDate": 1798819199000i64,
                                "semesterGoal": 1.0,
                                "semesterValid": 0,
                                "goalMethod": 2
                            },
                            "goal": {
                                "targetDis": 1.0,
                                "targetCount": 0,
                                "daysLeft": 45,
                                "inTerm": true
                            }
                        }
                    }))
                }
            }),
        )
        .route(
            "/api/records",
            get(|| async {
                axum::Json(json!({
                    "ok": true,
                    "data": {
                        "records": [{
                            "rrid": 1367213418,
                            "uuid": "6674FB31",
                            "sportType": 1,
                            "totalTime": 426,
                            "totalDis": 1212.0,
                            "validDis": 1212.0,
                            "validTime": 426,
                            "startTime": 1794870446000i64,
                            "stopTime": 1794870872000i64,
                            "complete": true,
                            "status": 0,
                            "appealStatus": 0,
                            "stepFreqStatus": 0,
                            "avgStepFreq": 158,
                            "distanceKm": 1.21
                        }, {
                            // 事后复查用的那条：4 天前跑的，落在复查窗口里。
                            "rrid": 1371568189,
                            "uuid": "AA11BB22",
                            "sportType": 1,
                            "totalTime": 900,
                            "totalDis": 1982.0,
                            "validDis": 1982.0,
                            "validTime": 900,
                            "startTime": (ru41::util::now() - 4 * 86_400) * 1000,
                            "stopTime": (ru41::util::now() - 4 * 86_400 + 900) * 1000,
                            "complete": true,
                            "status": 1,
                            "appealStatus": 4,
                            "stepFreqStatus": 0,
                            "avgStepFreq": 160,
                            "distanceKm": 1.98
                        }]
                    }
                }))
            }),
        )
        .route(
            "/api/cheat",
            get(|| async {
                axum::Json(json!({
                    "ok": true,
                    "data": {
                        "status": "no_record",
                        "label": "暂未发现违规记录",
                        "count": null,
                        "reason": "服务端未提供原因",
                        "appeal": "未知",
                        "occurredAt": "服务端未提供时间",
                        "rrid": null,
                        "checkedAtMs": 1794871590236i64,
                        "listCount": 9
                    }
                }))
            }),
        )
        .route(
            "/api/record/1371568189",
            get(|| async {
                axum::Json(json!({
                    "ok": true,
                    "data": {
                        "rrid": 1371568189,
                        "status": 1,
                        "statusInfo": "你的跑步成绩已被系统判为可疑",
                        "isCheat": 1,
                        "canAppeal": 0,
                        "appealStatusInfo": "申诉未通过，查看审核信息",
                        "totalDis": 1220.0,
                        "validDis": 1220.0,
                        "totalTime": 900,
                        "validTime": 900,
                        "startTime": 1795168519000i64,
                        "stopTime": 1795169419000i64,
                        "complete": true,
                        "reasonList": [
                            {"type": 0, "reason": "运动数据有问题", "complete": false},
                            {"type": 1, "reason": "按顺序通过所有点位", "complete": true},
                            {"type": 1, "reason": "最少要跑1.00公里", "complete": true}
                        ]
                    }
                }))
            }),
        )
        .route(
            "/api/boom",
            get(|| async { axum::Json(json!({"ok": false, "error": "not found"})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), seen)
}

/// 测试用配置段（令牌直接写在配置里，避免测试之间抢环境变量）。
fn section(base_url: &str) -> CampusrunSection {
    CampusrunSection {
        base_url: Some(base_url.to_owned()),
        token_env: Some("CAMPUSRUN_TEST_TOKEN".into()),
        token: Some("secret-token".into()),
        timeout_secs: Some(5),
        ..Default::default()
    }
}

#[test]
fn section_reads_token_from_env_when_inline_missing() {
    let mut section = section("http://127.0.0.1:1");
    section.token = None;
    section.token_env = Some("CAMPUSRUN_ENV_ONLY_TOKEN".into());
    // 环境变量没设 → 没有令牌。
    assert_eq!(section.token(), None);
    assert!(!section.is_ready());
    unsafe { std::env::set_var("CAMPUSRUN_ENV_ONLY_TOKEN", "env-token") };
    assert_eq!(section.token().as_deref(), Some("env-token"));
    assert!(section.is_ready());
    unsafe { std::env::remove_var("CAMPUSRUN_ENV_ONLY_TOKEN") };
}

#[test]
fn inline_token_wins_over_env() {
    let section = section("http://127.0.0.1:1");
    assert_eq!(section.token().as_deref(), Some("secret-token"));
}

#[test]
fn section_base_url_defaults_and_trims() {
    let mut section = section("http://127.0.0.1:9999/");
    assert_eq!(section.base_url(), "http://127.0.0.1:9999");
    section.base_url = None;
    assert_eq!(section.base_url(), CampusrunSection::DEFAULT_BASE_URL);
}

#[tokio::test]
async fn fetches_semester_records_and_cheat() {
    let (base_url, seen) = mock_console().await;
    let client = CampusrunClient::new(&section(&base_url)).unwrap();

    let info = client.semester().await.unwrap();
    assert_eq!(
        info.summary_raw.sname.as_deref(),
        Some("2026-2027-1体能训练Ⅰ")
    );
    assert_eq!(info.summary_raw.semester_valid_dis, Some(6295.0));
    // semesterGoal 是**目标里程（米）**，不是次数（2026-11-16 修正）。
    assert_eq!(
        info.goal_raw.as_ref().unwrap().semester_goal,
        Some(1.0),
        "学期目标里程应来自 goalRaw 的 semesterGoal（米）"
    );

    let records = client.records().await.unwrap();
    // 两条：一条老记录 + 一条用于事后复查的（4 天前跑的）。
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].rrid, 1367213418);
    assert_eq!(records[0].avg_step_freq, Some(158));
    assert_eq!(records[1].rrid, 1371568189);

    let cheat = client.cheat().await.unwrap();
    assert_eq!(cheat.status, "no_record");
    assert_eq!(cheat.list_count, Some(9));

    // 令牌确实带上去了。
    let tokens = seen.lock().await;
    assert_eq!(tokens.first().map(String::as_str), Some("secret-token"));
}

#[tokio::test]
async fn renders_markdown_for_each_view() {
    let (base_url, _seen) = mock_console().await;
    let client = CampusrunClient::new(&section(&base_url)).unwrap();

    let status = ru41_plugin_campusrun::service::fetch_status(&client)
        .await
        .unwrap();
    assert!(status.contains("## 校园跑进度"), "{status}");
    // 插件层不许自造「学期目标」（55c6437 撤掉了）→ 没给公里目标就明说，
    // 并且不许说「已达标」。
    assert!(
        status.contains("学期目标：**服务端未给出公里目标**"),
        "{status}"
    );
    assert!(status.contains("不能据此说「已达标」"), "{status}");
    assert!(status.contains("有效里程：6.29 km"), "{status}");
    assert!(status.contains("学期剩余：45 天"), "{status}");

    let records = ru41_plugin_campusrun::service::fetch_records(&client, 5)
        .await
        .unwrap();
    assert!(records.contains("## 校园跑最近记录"), "{records}");
    assert!(records.contains("5'51\"/km"), "{records}");

    let cheat = ru41_plugin_campusrun::service::fetch_cheat(&client)
        .await
        .unwrap();
    assert!(cheat.contains("你的违规记录：暂未发现违规记录"), "{cheat}");
    // 全校通报榜人数不是你的违规数（服务端 self 与 list 是两回事）。
    assert!(cheat.contains("全校违规通报榜：9 人"), "{cheat}");

    let all = ru41_plugin_campusrun::service::fetch_all(&client)
        .await
        .unwrap();
    assert!(all.contains("## 校园跑进度"), "{all}");
    assert!(all.contains("## 校园跑最近记录"), "{all}");
    assert!(all.contains("## 校园跑账号状态"), "{all}");
}

#[tokio::test]
async fn missing_token_fails_before_any_request() {
    let (base_url, _seen) = mock_console().await;
    let mut section = section(&base_url);
    section.token = None;
    section.token_env = Some("CAMPUSRUN_ABSENT_TOKEN".into());
    let client = CampusrunClient::new(&section).unwrap();
    assert!(!client.has_token());
    let error = client.semester().await.unwrap_err();
    assert!(error.to_string().contains("令牌"), "{error}");
}

#[tokio::test]
async fn api_error_is_surfaced() {
    let (base_url, _seen) = mock_console().await;
    let client = CampusrunClient::new(&section(&base_url)).unwrap();
    let error = client
        .get_json::<serde_json::Value>("/api/boom")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

// ── 事后复查（2026-11-26）────────────────────────────────────────────

/// 复查用的假渠道：把发出去的消息记下来。
struct MockChannel {
    sent: std::sync::Mutex<Vec<String>>,
}

impl ru41::ports::channel::NotifyChannel for MockChannel {
    fn channel_id(&self) -> &str {
        "mock"
    }

    fn send<'a>(
        &'a self,
        target: &'a str,
        _msg_id: Option<&'a str>,
        text: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.sent.lock().unwrap().push(format!("{target}|{text}"));
            Ok(())
        })
    }
}

/// 提交后第 3 天起：把窗口内的记录拉出来看有没有被回扫判违规；判了就推一条。
///
/// 复查以**服务端的记录列表**为准（不靠本地「上次提交」台账），
/// 所以第二次提交不会挤掉第一次、App 里提交的记录也查得到。
///
/// 这条链路在生产机要登录 pace-forge 才跑得通（2026-11-26 实测：报「请先登录」），
/// 所以这里用假控制台把整条链路钉住。
#[tokio::test]
async fn review_flags_a_backswept_record_and_notifies_once() {
    let (base_url, _seen) = mock_console().await;
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ru41::ports::storage::TaskStore> = Arc::new(
        ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db")).unwrap(),
    );

    let client = CampusrunClient::new(&section(&base_url)).unwrap();
    let channel = Arc::new(MockChannel {
        sent: std::sync::Mutex::new(Vec::new()),
    });
    let notifier: Arc<dyn ru41::ports::channel::NotifyChannel> = channel.clone();
    let tracker = ru41_plugin_campusrun::service::CampusrunTracker::with_review(
        client,
        store.clone(),
        Some(notifier),
        "user:test".to_owned(),
    );
    // 拉一次（mock 控制台把学期/记录/违规都准备了）。
    let _ = <_ as ru41::ingress::schedule::SimplePollJob>::poll(&tracker).await;

    // 1371568189（4 天前跑的，落在复查窗口里）被服务端判了违规 ⇒ 落了账 + 推了一条。
    let doc = store
        .document("campusrun.review", "1371568189")
        .unwrap()
        .expect("复查查到了就必须落账");
    assert_eq!(doc.value["flagged"], serde_json::json!(true));
    assert_eq!(
        doc.value["cheat_reasons"],
        serde_json::json!(["运动数据有问题"]),
        "{:?}",
        doc.value
    );
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "同一个 rrid 只推一次: {sent:?}");
        assert!(sent[0].contains("运动数据有问题"), "{sent:?}");
        assert!(sent[0].contains("1371568189"), "{sent:?}");
    }

    // 再轮询一次：**同一个 rrid 不许再推**。
    let _ = <_ as ru41::ingress::schedule::SimplePollJob>::poll(&tracker).await;
    assert_eq!(
        channel.sent.lock().unwrap().len(),
        1,
        "同一个 rrid 只推一次"
    );
}

/// 「先干净、后来被判违规」的假控制台：记录详情端点**第一次**返回干净，
/// 之后再返回被判违规（模拟上游 issue #51 的回扫：第 4~7 天才判）。
async fn mock_console_staged_review() -> (String, Arc<Mutex<Vec<String>>>) {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/api/records",
            get(|| async {
                axum::Json(json!({
                    "ok": true,
                    "data": {
                        "records": [{
                            "rrid": 1371568189,
                            "uuid": "AA11BB22",
                            "sportType": 1,
                            "totalTime": 900,
                            "totalDis": 1982.0,
                            "validDis": 1982.0,
                            "validTime": 900,
                            "startTime": (ru41::util::now() - 4 * 86_400) * 1000,
                            "stopTime": (ru41::util::now() - 4 * 86_400 + 900) * 1000,
                            "complete": true,
                            "status": 0,
                            "appealStatus": 0,
                            "stepFreqStatus": 0,
                            "avgStepFreq": 160,
                            "distanceKm": 1.98
                        }]
                    }
                }))
            }),
        )
        .route(
            "/api/record/1371568189",
            get(move || {
                let hits = hits.clone();
                async move {
                    let n = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // 第一次干净，之后被判违规。
                    let (is_cheat, cheat_complete, status_info, can_appeal) = if n == 0 {
                        (0, true, "", 1)
                    } else {
                        (1, false, "你的跑步成绩已被系统判为可疑", 0)
                    };
                    axum::Json(json!({
                        "ok": true,
                        "data": {
                            "rrid": 1371568189,
                            "status": is_cheat,
                            "statusInfo": status_info,
                            "isCheat": is_cheat,
                            "canAppeal": can_appeal,
                            "appealStatusInfo": if is_cheat == 1 { "申诉未通过，查看审核信息" } else { "" },
                            "totalDis": 1982.0,
                            "validDis": 1982.0,
                            "totalTime": 900,
                            "validTime": 900,
                            "startTime": (ru41::util::now() - 4 * 86_400) * 1000,
                            "stopTime": (ru41::util::now() - 4 * 86_400 + 900) * 1000,
                            "complete": true,
                            "reasonList": [
                                {"type": 0, "reason": "运动数据有问题", "complete": cheat_complete},
                                {"type": 1, "reason": "按顺序通过所有点位", "complete": true},
                                {"type": 1, "reason": "最少要跑1.00公里", "complete": true}
                            ]
                        }
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), seen)
}

/// **回归**：第 3 天查的时候还没判违规 ⇒ 不能就此了事，要继续查到第 4~7 天。
///
/// 旧实现是「查了就记账、之后直接跳过」⇒ 第 3 天干净、第 5 天才被判违规的
/// 那条**永远不会被通知**（上游 issue #51 说的正是这种回扫）。
#[tokio::test]
async fn review_rechecks_a_clean_record_and_catches_a_late_flag() {
    let (base_url, _seen) = mock_console_staged_review().await;
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn ru41::ports::storage::TaskStore> = Arc::new(
        ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("s.db")).unwrap(),
    );
    let client = CampusrunClient::new(&section(&base_url)).unwrap();
    let channel = Arc::new(MockChannel {
        sent: std::sync::Mutex::new(Vec::new()),
    });
    let notifier: Arc<dyn ru41::ports::channel::NotifyChannel> = channel.clone();
    let now_ms = ru41::util::now() * 1000;

    // 第 3 天：查了，没判违规 ⇒ 记一笔，但**不推**。
    let outcomes = ru41_plugin_campusrun::review::review_recent_records(
        &client,
        store.as_ref(),
        Some(&notifier),
        "user:test",
        now_ms,
    )
    .await
    .unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert!(!outcomes[0].flagged, "{outcomes:?}");
    assert!(
        channel.sent.lock().unwrap().is_empty(),
        "没判违规就不该推"
    );
    let doc = store
        .document("campusrun.review", "1371568189")
        .unwrap()
        .expect("查过就要落账");
    assert_eq!(doc.value["flagged"], serde_json::json!(false));

    // 同一天再查一次：间隔不到 24 小时 ⇒ 不查、不推。
    let outcomes = ru41_plugin_campusrun::review::review_recent_records(
        &client,
        store.as_ref(),
        Some(&notifier),
        "user:test",
        now_ms,
    )
    .await
    .unwrap();
    assert!(outcomes.is_empty(), "24 小时内不该重复查: {outcomes:?}");
    assert!(channel.sent.lock().unwrap().is_empty());

    // 第 4 天：服务端判了违规 ⇒ **补推一条**（这正是旧实现漏掉的）。
    let outcomes = ru41_plugin_campusrun::review::review_recent_records(
        &client,
        store.as_ref(),
        Some(&notifier),
        "user:test",
        now_ms + 86_400 * 1000,
    )
    .await
    .unwrap();
    assert_eq!(outcomes.len(), 1, "{outcomes:?}");
    assert!(outcomes[0].flagged, "{outcomes:?}");
    {
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "同一个 rrid 只推一次: {sent:?}");
        assert!(sent[0].contains("运动数据有问题"), "{sent:?}");
    }
    let doc = store
        .document("campusrun.review", "1371568189")
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["flagged"], serde_json::json!(true));
}
