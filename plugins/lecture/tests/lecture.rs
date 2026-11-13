//! lecture 插件集成测试：handler 拦截判定、下载入队、课程匹配。

use chrono::TimeZone;
use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    app::schedule::ScheduleService,
    domain::{schedule::EventInput, schedule::EventRecurrence, todo::TodoOptions},
    ports::channel::{MediaKind, MediaRef},
    ports::commands::{CommandContext, CommandHandler},
    ports::storage::TaskStore,
};
use ru41_plugin_lecture::{LectureConfig, LectureHandler, LectureSection, LectureWorker};
use std::{
    future::Future,
    io::{BufRead, Write},
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
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
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
        auth: None,
    }]
}

fn worker(store: Arc<dyn TaskStore>, tmp: &tempfile::TempDir) -> LectureWorker {
    LectureWorker::new(
        store.clone(),
        TodoOptions::default(),
        config(tmp.path().to_path_buf()),
    )
}

/// 转写产出后不能把整个会话目录删掉：2026-11-09 丢过一节网络空间安全导论的
/// 转写（连原始录音一起没了，笔记只能写「待补」）。切片立刻删（磁盘大头），
/// 原始音频按保留期留，转写文本留着。
#[test]
fn cleanup_keeps_transcript_and_audio_until_retention() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
    let worker = worker(store, &tmp);

    let work = tmp.path().join("m1");
    std::fs::create_dir_all(work.join("slices")).unwrap();
    std::fs::create_dir_all(work.join("raw")).unwrap();
    std::fs::write(work.join("slices").join("a.mp3"), b"x").unwrap();
    std::fs::write(work.join("raw").join("a.mp3"), b"x").unwrap();
    std::fs::write(work.join("transcript.md"), "老师讲了导数").unwrap();

    let item = ru41_plugin_lecture::pending::PendingItem {
        file: String::new(),
        filename: "a.mp3".into(),
        size: 1,
        channel: "qq".into(),
        target: "u".into(),
        msg_id: "m1".into(),
        received_at: ru41::util::now(),
        status: "done".into(),
        error: None,
        course: None,
        transcript: Some("老师讲了导数".into()),
    };

    // 刚转写完：切片删掉（磁盘大头），原始音频与转写文本都留着。
    worker.cleanup_workspace(&work, &item, ru41::util::now());
    assert!(!work.join("slices").exists(), "切片应立即删除");
    assert!(work.join("raw").exists(), "原始音频应按保留期留着");
    assert!(work.join("transcript.md").exists(), "转写文件应留着");

    // 过保留期（默认 7 天）：原始音频也删，转写文本仍在。
    let mut aged = item.clone();
    aged.received_at = ru41::util::now() - 8 * 86_400;
    worker.cleanup_workspace(&work, &aged, ru41::util::now());
    assert!(!work.join("raw").exists(), "过保留期应删除原始音频");
    assert!(work.join("transcript.md").exists(), "转写文件应留着");
}

#[test]
fn non_audio_attachments_are_not_intercepted() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()), TodoOptions::default());
    let atts = vec![MediaRef::new(MediaKind::File, "https://x/doc.pdf")];
    let cx = ctx(&store, &atts, "user:U1", "U1", "m1", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert!(reply.is_none());
}

/// 在同步测试线程里跑 async fn：plugin 侧不能自带 runtime，
/// 测试同样只在 block_on 语义下验证（宿主 dispatch 已在 block_in_place）。
fn tokio_bridge_block<F: Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(f)
}

#[test]
fn unbound_or_group_is_deflected() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()), TodoOptions::default());
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
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()), TodoOptions::default());
    let url = mock_file_server(AUDIO);
    let atts = audio_attachment(&url);
    bind(&store);
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-audio", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    let text = reply.unwrap().text;
    // 2026-11-13 起只去掉「《xxx.mp3》（xx.x MB），分段识别中（每段约 5 分钟）」那句，
    // 其余照旧：课程名 + 要做什么 + 队列。
    assert!(text.contains("这节课已结束"), "{text}");
    assert!(text.contains("课表里没对上"), "{text}");
    assert!(!text.contains("录音"), "{text}");
    assert!(!text.contains("分段识别中"), "{text}");
    assert!(!text.contains("MB"), "{text}");
    // 文件已落盘，pending 文档已登记。
    let docs = store.documents("lecture.pending", 10).unwrap();
    assert_eq!(docs.len(), 1);
    let item = ru41_plugin_lecture::pending::PendingItem::from_document(&docs[0]).unwrap();
    assert_eq!(item.status, "pending");
    assert_eq!(
        PathBuf::from(&item.file).file_name().unwrap(),
        "高数 03-12.m4a"
    );
    assert_eq!(std::fs::read(&item.file).unwrap(), AUDIO);
    // 重复投递（同 msg_id）不覆盖文档。
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-audio", true);
    tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    assert_eq!(store.documents("lecture.pending", 10).unwrap().len(), 1);
}

/// 回执全文快照：整条消息逐字断言，不只看 contains。
///
/// 模板改版时（如 2026-11-13 去掉「分段识别中」那句）靠这个用例锁住
/// 「用户到底会看到什么」——含标题行、空行、队列行，一字不差。
#[test]
fn receipt_text_is_exact() {
    // ① 课表对得上：点名课程
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
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
    let url = mock_file_server(AUDIO);
    let local = chrono::Local.timestamp_opt(t0 + 600, 0).single().unwrap();
    let filename = format!(
        "{}年{}月{}日 {}点{}分.m4a",
        local.format("%Y"),
        local.format("%m"),
        local.format("%d"),
        local.format("%H"),
        local.format("%M")
    );
    let atts = vec![MediaRef {
        kind: MediaKind::File,
        source: url,
        filename: Some(filename),
        auth: None,
    }];
    bind(&store);
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()), TodoOptions::default());
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-exact", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    let matched = reply.unwrap().text;
    assert_eq!(
        matched,
        "## 高等数学 这节课已结束\n\n\
         开始分析这节课并整理成课堂笔记，完成后写进 Vault 并通知你。\n\
         - 队列：就这一节，不用等"
    );

    // ② 课表对不上：说清楚先放收件箱
    let tmp2 = tempfile::tempdir().unwrap();
    let store2: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp2.path().join("state.db")).unwrap());
    let url2 = mock_file_server(AUDIO);
    let atts2 = audio_attachment(&url2);
    bind(&store2);
    let handler2 = LectureHandler::new(config(tmp2.path().to_path_buf()), TodoOptions::default());
    let cx2 = ctx(&store2, &atts2, "user:U1", "U1", "m-exact2", true);
    let reply2 = tokio_bridge_block(async { handler2.handle_message("", &cx2).await.unwrap() });
    let unmatched = reply2.unwrap().text;
    assert_eq!(
        unmatched,
        "## 这节课已结束\n\n\
         开始整理课堂笔记；课表里没对上这节课的时间，笔记先放收件箱，等你确认归到哪门课。\n\
         - 队列：就这一节，不用等"
    );

    // 供人眼核对：这两条就是 QQ 里实际收到的样子
    println!("\n===== 回执全文（课表对得上）=====\n{matched}\n");
    println!("===== 回执全文（课表对不上）=====\n{unmatched}\n");
}

/// 回执点名课程（2026-11-13 起只去掉「分段识别中」那句，点名课程照旧）。
/// 课程匹配本身由 worker 侧负责并有直接测试（course_matching_by_interval_only）。
#[test]
fn receipt_names_the_matched_course() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
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
    let url = mock_file_server(AUDIO);
    // 文件名带上课时刻（与课表区间重合）→ 回执直接点名课程。
    let local = chrono::Local.timestamp_opt(t0 + 600, 0).single().unwrap();
    let filename = format!(
        "{}年{}月{}日 {}点{}分.m4a",
        local.format("%Y"),
        local.format("%m"),
        local.format("%d"),
        local.format("%H"),
        local.format("%M")
    );
    let atts = vec![MediaRef {
        kind: MediaKind::File,
        source: url,
        filename: Some(filename),
        auth: None,
    }];
    bind(&store);
    let handler = LectureHandler::new(config(tmp.path().to_path_buf()), TodoOptions::default());
    let cx = ctx(&store, &atts, "user:U1", "U1", "m-course", true);
    let reply = tokio_bridge_block(async { handler.handle_message("", &cx).await.unwrap() });
    let text = reply.unwrap().text;
    assert!(text.contains("高等数学 这节课已结束"), "{text}");
    assert!(text.contains("开始分析这节课"), "{text}");
    assert!(!text.contains("分段识别中"), "{text}");
}

#[test]
fn course_matching_by_interval_only() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
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
    // 开课前 5 分钟起录 → 容差内，仍算该课。
    assert_eq!(w.course_at(t0 - 300), "高等数学");
    // 下课后不再就近猜课（猜错课比留兜底更伤归档）。
    assert_eq!(w.course_at(t0 + 5400 + 300), "课堂录音");
    // 窗口外 → fallback。
    assert_eq!(w.course_at(t0 + 5 * 3600), "课堂录音");
    // 日程为空 → fallback。
    let tmp2 = tempfile::tempdir().unwrap();
    let store2 = Arc::new(SqliteTaskStore::open(&tmp2.path().join("state.db")).unwrap());
    assert_eq!(worker(store2, &tmp2).course_at(t0), "课堂录音");
}

#[test]
fn note_target_is_absolute_and_falls_back_to_inbox() {
    let tmp = tempfile::tempdir().unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&tmp.path().join("state.db")).unwrap());
    let w = worker(store, &tmp);
    // 认得出课程：绝对路径 + 课程根（目录名由 agent 查课程总览对照表）。
    let target = w.note_target("思想道德与法治", "2026-11-05");
    assert!(target.starts_with("/vault/"), "{target}");
    assert!(target.contains("01-Courses-课程"), "{target}");
    assert!(target.ends_with("课堂笔记.md"), "{target}");
    // 认不出课程：落收件箱，不硬造课程目录。
    let target = w.note_target("课堂录音", "2026-11-05");
    assert_eq!(
        target,
        "/vault/00-Inbox-收件箱/待归类-课堂笔记-2026-11-05.md"
    );
}
