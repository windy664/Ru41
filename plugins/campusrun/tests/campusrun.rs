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
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].rrid, 1367213418);
    assert_eq!(records[0].avg_step_freq, Some(158));

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

/// 提交后第 3 天：把那条记录拉出来看有没有被回扫判违规；判了就推一条。
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
    // 4 天前提交过一条（正好超过 3 天的复查门槛）。
    let four_days_ago = ru41::util::now() - 4 * 86_400;
    store
        .put_document(
            "campusrun.submit",
            "last-submit",
            None,
            &json!({"last_submit_at": four_days_ago, "rrid": 1371568189}),
        )
        .unwrap();

    let client = CampusrunClient::new(&section(&base_url)).unwrap();
    let channel = Arc::new(MockChannel {
        sent: std::sync::Mutex::new(Vec::new()),
    });
    let tracker = ru41_plugin_campusrun::service::CampusrunTracker::with_review(
        client,
        store.clone(),
        Some(channel.clone()),
        "user:test".to_owned(),
    );
    // 拉一次（mock 控制台只准备了 /api/semester 等端点；
    // 这里只关心复查那一段能不能跑通并落账）。
    let _ = <_ as ru41::ingress::schedule::SimplePollJob>::poll(&tracker).await;

    // 复查要么因为记录端点没有 mock 而没查成（不记账），
    // 要么查成了并落了账。两种都不该 panic —— 但要能查的路径是有账的。
    // 这里只断言「复查不会把没查到的当成查过」。
    let review = store
        .document("campusrun.review", "1371568189")
        .unwrap()
        .expect("复查查到了就必须落账");
    {
        let doc = review;
        // 查到了：必须记下「被判违规」和理由。
        assert_eq!(doc.value["flagged"], serde_json::json!(true));
        assert_eq!(
            doc.value["cheat_reasons"],
            serde_json::json!(["运动数据有问题"]),
            "{:?}",
            doc.value
        );
        // 且只推一次。
        let sent = channel.sent.lock().unwrap();
        assert_eq!(sent.len(), 1, "同一个 rrid 只推一次: {sent:?}");
        assert!(sent[0].contains("运动数据有问题"), "{sent:?}");
        assert!(sent[0].contains("1371568189"), "{sent:?}");
    }
}
