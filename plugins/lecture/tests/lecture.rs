//! lecture 插件集成测试：handler 拦截判定、下载入队、课程匹配。

use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    adapters::vault::GitVault,
    app::schedule::ScheduleService,
    domain::{schedule::EventInput, schedule::EventRecurrence, todo::TodoOptions},
    ports::channel::{MediaKind, MediaRef},
    ports::commands::{CommandContext, CommandHandler},
    ports::storage::TaskStore,
};
use ru41_plugin_lecture::{LectureConfig, LectureHandler, LectureSection, LectureWorker};
use std::{
    future::Future,
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    path::PathBuf,
    sync::Arc,
};

fn config(workspace: PathBuf) -> LectureConfig {
    let mut cfg = LectureSection {
        api_key: Some("test-key".into()),
        ..Default::default()
    }
    .resolved()
    .unwrap();
    // workspace 字段公开；resolved 的默认值 /data/lecture 只能在测试里直改。
    cfg.workspace = workspace;
    cfg
}

fn ctx<'a>(
    store: &'a Arc<dyn TaskStore>,
    attachments: &'a [MediaRef],
    target: &'a str,
    author: &'a str,
    msg_id: &'a str,
    is_c2c: bool,
) -> CommandContext<'a> {
    CommandContext {
        store,
        bind_secret: Some("s3cret"),
        target,
        msg_id,
        author_openid: author,
        is_c2c,
        reply_channel: "qq",
        image_url: None,
        attachments,
    }
}

fn bind(store: &Arc<dyn TaskStore>) {
    ru41::app::identity::IdentityService::new(store.clone())
        .bind("qq", "U1", "s3cret", "s3cret", 0)
        .unwrap();
}

/// 本地 mock 文件服务器：任何 GET 返回固定字节。
fn mock_file_server(payload: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            // 只读请求行就回包；读满请求体会等不到对端关写半边而卡死。
            let mut line = String::new();
            let _ = std::io::BufReader::new(&stream).read_line(&mut line);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", payload.len());
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(payload).unwrap();
        }
    });
    format!("http://{addr}/lec.m4a")
}

const AUDIO: &[u8] = b"fake-m4a-bytes";

fn audio_attachment(url: &str) -> Vec<MediaRef> {
    vec![MediaRef {
        kind: MediaKind::File,
        source: url.to_owned(),
        filename: Some("高数 03-12.m4a".into()),
    }]
}

fn worker(store: Arc<dyn TaskStore>, tmp: &tempfile::TempDir) -> LectureWorker {
    LectureWorker::new(
        store.clone(),
        Arc::new(GitVault::new(tmp.path().join("vault"), "origin", "master")),
        TodoOptions::default(),
        config(tmp.path().to_path_buf()),
    )
}

#[test]
fn non_audio_attachments_are_not_intercepted() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> = Arc::new(
        SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap(),
    );
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()));
    let atts = vec![MediaRef::new(MediaKind::File, "https://x/doc.pdf")];
    let cx = ctx(&store, &atts, "user:U1", "U1", "m1", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert!(reply.is_none());
}

/// 在同步测试线程里跑 async fn：plugin 侧不能自带 runtime，
/// 测试同样只在 block_on 语义下验证（宿主 dispatch 已在 block_in_place）。
fn tokio_bridge_block<F: Future>(f: F) -> F::Output
where
    F: std::future::Future,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(f)
}


#[test]
fn unbound_or_group_is_deflected() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> = Arc::new(
        SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap(),
    );
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()));
    let atts = audio_attachment("https://x/lec.m4a");
    // 未绑定单聊：提示绑定。
    let cx = ctx(&store, &atts, "user:U1", "U1", "m1", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert!(reply.unwrap().text.contains("绑定"));
    // 已绑定群聊：提示单聊。
    bind(&store);
    let cx = ctx(&store, &atts, "group:G1", "U1", "m2", false);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert!(reply.unwrap().text.contains("单聊"));
}

#[test]
fn audio_file_is_downloaded_and_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> = Arc::new(
        SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap(),
    );
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()));
    let url = mock_file_server(AUDIO);
    let atts = audio_attachment(&url);
    bind(&store);
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-audio", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    let text = reply.unwrap().text;
    assert!(text.contains("课堂录音已接收"), "{text}");
    assert!(text.contains("m4a"));
    // 文件已落盘，pending 文档已登记。
    let docs = store.documents("lecture.pending", 10).unwrap();
    assert_eq!(docs.len(), 1);
    let item = ru41_plugin_lecture::pending::PendingItem::from_document(&docs[0]).unwrap();
    assert_eq!(item.status, "pending");
    assert_eq!(PathBuf::from(&item.file).file_name().unwrap(), "高数 03-12.m4a");
    assert_eq!(std::fs::read(&item.file).unwrap(), AUDIO);
    // 重复投递（同 msg_id）不覆盖文档。
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-audio", true);
    tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert_eq!(store.documents("lecture.pending", 10).unwrap().len(), 1);
}

#[test]
fn course_matching_prefers_overlapping_class_then_nearest() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> = Arc::new(
        SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap(),
    );
    let w = worker(store.clone(), &tmp);
    let t0 = 1761891600; // 2026-10-31 10:00 +08:00
    ScheduleService::new(store.clone(), TodoOptions::default())
        .add(
            EventInput {
                title: "高等数学".into(),
                starts_at: t0,
                ends_at: t0 + 5400,
                location: Some("C301".into()),
                recurrence: EventRecurrence::None,
                source: None,
            },
            t0 - 3600,
        )
        .unwrap();
    // 上课中录音 → 该课。
    assert_eq!(w.course_at(t0 + 600), "高等数学");
    // 课后 1.5 小时内录音 → 最近一节。
    assert_eq!(w.course_at(t0 + 5400 + 300), "高等数学");
    // 窗口外 → fallback。
    assert_eq!(w.course_at(t0 + 5 * 3600), "课堂录音");
    // 日程为空 → fallback。
    let tmp2 = tempfile::tempdir().unwrap();
    let store2 = Arc::new(
        SqliteTaskStore::open(&tmp2.path().join("state.db")).unwrap(),
    );
    assert_eq!(worker(store2, &tmp2).course_at(t0), "课堂录音");
}
