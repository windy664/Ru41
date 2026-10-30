use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::NotifyRoute,
    modules::youtube::{YoutubeConfig, YoutubeTracker},
    ports::storage::TaskStore,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// 列表条目样例：vid-1 长视频（新）、vid-2 短视频。时间戳取当前时间（新鲜）。
const LIST_A: &str = concat!(
    "printf 'vid-1\\037Real AI News Deep Dive\\037306\\0371700000\\037%s\\n' \"$(date +%s)\"\n",
    "printf 'vid-2\\037Short clip about cats\\03730\\0375000\\037%s\\n' \"$(date +%s)\"\n",
);

/// 第二轮列表：vid-0 为基线之后新出现的视频，vid-1 已登记。
const LIST_B: &str = concat!(
    "printf 'vid-0\\037Brand new AI essay video\\037612\\03742000\\037%s\\n' \"$(date +%s)\"\n",
    "printf 'vid-1\\037Real AI News Deep Dive\\037306\\0371700000\\037%s\\n' \"$(date +%s)\"\n",
);

/// 仅 vid-1（长视频），供转写失败测试从基线后新出现。
const LIST_ONE: &str =
    "printf 'vid-1\\037Real AI News Deep Dive\\037306\\0371700000\\037%s\\n' \"$(date +%s)\"\n";

/// 仅 vid-2（短视频）作首见基线；后续轮次不含它，避免干扰候选断言。
const LIST_SHORT: &str =
    "printf 'vid-2\\037Short clip about cats\\03730\\0375000\\037%s\\n' \"$(date +%s)\"\n";

/// 带关键词命中的第二轮列表：vid-1 命中，vid-3 不命中（预期 filtered）。
const LIST_KEYWORD: &str = concat!(
    "printf 'vid-1\\037Real AI News Deep Dive\\037306\\0371700000\\037%s\\n' \"$(date +%s)\"\n",
    "printf 'vid-3\\037Cooking vlog episode\\037480\\0379000\\037%s\\n' \"$(date +%s)\"\n",
);

/// 老视频测试列表：flat 时间戳为 NA（真实 flat 行为），逼出 meta 兜底路径。
const LIST_OLD: &str = "printf 'vid-old\\037Some AI essay video\\037306\\0379000\\037NA\\n'\n";

/// 假 yt-dlp：按参数判模式——`--flat-playlist` 输出 LIST_LINES；`--sub-langs` 为字幕模式
/// （MARKER 存在时失败，验证转写重试与放弃路径）；否则为 meta 模式（vid-old 固定老时间戳，
/// 其余返回当前时间）。
/// 每次调用写一个新文件，不复用路径：避免「改写下轮列表内容」与
/// 「上一轮脚本仍在被执行」在并行测试里相互干扰。
fn write_fake_ytdlp(dir: &Path, marker: &Path, list_lines: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let script = dir.join(format!(
        "fake-ytdlp-{}.sh",
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             mode=meta\n\
             for arg in \"$@\"; do\n\
             if [ \"$arg\" = \"--flat-playlist\" ]; then mode=list; fi\n\
             if [ \"$arg\" = \"--sub-langs\" ]; then mode=subs; fi\n\
             done\n\
             if [ \"$mode\" = list ]; then\n\
             {list_lines}\
             exit 0\n\
             fi\n\
             if [ \"$mode\" = subs ]; then\n\
             if [ -f \"{marker}\" ]; then\n\
             echo 'mock subtitle failure' >&2\n\
             exit 1\n\
             fi\n\
             prev=\"\"\n\
             o_value=\"\"\n\
             for arg in \"$@\"; do\n\
             if [ \"$prev\" = \"-o\" ]; then o_value=\"$arg\"; fi\n\
             prev=\"$arg\"\n\
             done\n\
             printf 'WEBVTT\\n\\n00:00:04.240 --> 00:00:06.230 align:start position:0%%\\nunveiled the Roadster and its ability to\\nfly<00:00:05.000><c> by</c><00:00:05.200><c> farting</c><00:00:05.600><c> cold</c><00:00:05.900><c> gas.</c>\\n\\n00:00:06.230 --> 00:00:06.240 align:start position:0%%\\nfly by farting cold gas.\\n\\n00:01:02.000 --> 00:01:04.000\\ntrained from scratch by hand\\n' > \"${{o_value}}.en.vtt\"\n\
             exit 0\n\
             fi\n\
             for arg in \"$@\"; do\n\
             case \"$arg\" in\n\
             *watch?v=vid-old*)\n\
             printf 'Old Video\\037306\\0371700000000\\n'\n\
             exit 0;;\n\
             esac\n\
             done\n\
             printf 'Meta Title\\037306\\037'\"$(date +%s)\"'\\n'\n\
             exit 0\n",
            marker = marker.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

fn tracker(
    store: Arc<dyn TaskStore>,
    dir: &Path,
    marker: &Path,
    list_lines: &str,
) -> YoutubeTracker {
    tracker_with(store, dir, marker, Vec::new(), 14, list_lines)
}

fn tracker_with(
    store: Arc<dyn TaskStore>,
    dir: &Path,
    marker: &Path,
    keywords: Vec<String>,
    max_age_days: u64,
    list_lines: &str,
) -> YoutubeTracker {
    let config = YoutubeConfig {
        channels: vec!["https://www.youtube.com/@test/videos".into()],
        keywords,
        interests: "AI / 安全".into(),
        level: "CET-6 / B2".into(),
        min_duration_secs: 60,
        max_per_channel: 15,
        max_per_poll: 5,
        max_age_days,
        max_transcript_chars: 24_000,
        interval_secs: 21_600,
        backend: "codex".into(),
        notify: Some(NotifyRoute {
            channel: "qq".into(),
            target: "user:U1".into(),
            msg_id: None,
        }),
        digest_at: "12:10".into(),
        ytdlp_bin: write_fake_ytdlp(dir, marker, list_lines)
            .to_string_lossy()
            .to_string(),
        proxy_url: None,
        ytdlp_cookies: None,
    };
    YoutubeTracker::new(store, config)
}

fn seen_status(store: &Arc<dyn TaskStore>, video_id: &str) -> String {
    store
        .document(
            "youtube_seen",
            &format!("https://www.youtube.com/@test/videos#{video_id}"),
        )
        .unwrap()
        .unwrap()
        .value["status"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn baseline_snapshot_then_prepares_only_new_video() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("absent-marker");
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let first_round = tracker(store.clone(), dir.path(), &marker, LIST_A);

    // 首轮 = 首次接触频道：只登记存量快照，一个都不预习（杜绝历史积压刷屏）。
    let report = first_round.poll_once().await.unwrap();
    assert_eq!(report.fetched, 2);
    assert_eq!(report.new_videos, 2);
    assert_eq!(report.prepared, 0);
    assert_eq!(report.errors.len(), 0);
    assert_eq!(store.list(10).unwrap().len(), 0);
    assert_eq!(seen_status(&store, "vid-1"), "baseline");
    assert_eq!(seen_status(&store, "vid-2"), "baseline");
    assert!(
        store
            .document("youtube_baseline", "https://www.youtube.com/@test/videos")
            .unwrap()
            .is_some()
    );

    // 第二轮（换列表：vid-0 新出现）：只有新视频入队。
    let next = tracker(store.clone(), dir.path(), &marker, LIST_B);
    let report = next.poll_once().await.unwrap();
    assert_eq!(report.new_videos, 1);
    assert_eq!(report.prepared, 1);
    assert_eq!(report.task_ids.len(), 1);

    let tasks = store.list(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].request_key, "youtube:prep:vid-0");
    assert!(
        tasks[0]
            .prompt
            .contains("https://www.youtube.com/watch?v=vid-0")
    );
    // 转写带行号与时间戳标记，供预习单逐字照抄。
    assert!(
        tasks[0]
            .prompt
            .contains("[L1 0:04] unveiled the Roadster and its ability to")
    );
    assert!(tasks[0].prompt.contains("farting cold gas"));
    // 自动轮询的预习单不带即时通知——统一进每日日报。
    assert!(tasks[0].notify.is_none());
    assert_eq!(seen_status(&store, "vid-0"), "done");
    // 已登记的 vid-1 不受影响。
    assert_eq!(seen_status(&store, "vid-1"), "baseline");

    // 第三轮：全部已登记，不再入队。
    let report = next.poll_once().await.unwrap();
    assert_eq!(report.new_videos, 0);
    assert_eq!(report.prepared, 0);
    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[tokio::test]
async fn transcript_failure_retries_then_gives_up() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("fail-subs");
    std::fs::write(&marker, b"trigger").unwrap();
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    // 基线只含短视频；vid-1 在下一轮新出现，字幕拉取持续失败。
    let first = tracker(store.clone(), dir.path(), &marker, LIST_SHORT);
    first.poll_once().await.unwrap();

    let tracker = tracker(store.clone(), dir.path(), &marker, LIST_ONE);
    let seen_key = "https://www.youtube.com/@test/videos#vid-1";
    for attempt in 1..=3 {
        let report = tracker.poll_once().await.unwrap();
        assert_eq!(report.prepared, 0, "attempt {attempt}");
        assert!(!report.errors.is_empty());
        let seen = store.document("youtube_seen", seen_key).unwrap().unwrap();
        assert_eq!(seen.value["attempts"], attempt);
        let expected = if attempt < 3 { "pending" } else { "failed" };
        assert_eq!(seen.value["status"], expected);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // 放弃后不再重复登记或入队。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.prepared, 0);
    assert_eq!(store.list(10).unwrap().len(), 0);
}

#[tokio::test]
async fn keyword_filter_marks_filtered_before_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("absent-marker");
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let tracker = tracker(store.clone(), dir.path(), &marker, LIST_SHORT);
    tracker.poll_once().await.unwrap();

    let tracker = tracker_with(
        store.clone(),
        dir.path(),
        &marker,
        vec!["deep dive".into()],
        14,
        LIST_KEYWORD,
    );
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_videos, 2);
    assert_eq!(report.prepared, 1);
    assert_eq!(seen_status(&store, "vid-1"), "done");
    assert_eq!(seen_status(&store, "vid-3"), "filtered");
}

#[tokio::test]
async fn freshness_window_marks_old_without_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("absent-marker");
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let first = tracker(store.clone(), dir.path(), &marker, LIST_SHORT);
    first.poll_once().await.unwrap();

    // flat 列表缺时间戳（NA）→ 补 meta 得到老时间戳 → 只登记 old，不入队。
    let tracker = tracker(store.clone(), dir.path(), &marker, LIST_OLD);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_videos, 1);
    assert_eq!(report.prepared, 0);
    assert_eq!(report.errors.len(), 0);
    assert_eq!(store.list(10).unwrap().len(), 0);
    assert_eq!(seen_status(&store, "vid-old"), "old");
}

#[tokio::test]
async fn rebaseline_covers_preexisting_pending_docs() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("absent-marker");
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    // 模拟旧版本遗留：无基线文档 + 一条历史积压 pending。
    let channel = "https://www.youtube.com/@test/videos";
    store
        .put_document(
            "youtube_seen",
            &format!("{channel}#vid-1"),
            None,
            &serde_json::json!({
                "status": "pending",
                "attempts": 0,
                "title": "Real AI News Deep Dive",
                "first_seen": 0,
            }),
        )
        .unwrap();

    let tracker = tracker(store.clone(), dir.path(), &marker, LIST_A);
    let report = tracker.poll_once().await.unwrap();
    // 重基线覆盖存量状态：pending → baseline，永不入队。
    assert_eq!(report.prepared, 0);
    assert_eq!(report.errors.len(), 0);
    assert_eq!(seen_status(&store, "vid-1"), "baseline");
    let doc = store
        .document("youtube_seen", &format!("{channel}#vid-1"))
        .unwrap()
        .unwrap();
    assert_eq!(doc.value["title"], "Real AI News Deep Dive");
    assert!(store.list(10).unwrap().is_empty());
}

#[tokio::test]
async fn prep_video_task_creates_deduped_tasks() {
    use ru41::modules::youtube::{service::prep_video_task, source::YtDlpClient};

    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("absent-marker");
    let store: Arc<dyn TaskStore> =
        Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
    let client = YtDlpClient::new(
        write_fake_ytdlp(dir.path(), &marker, LIST_A)
            .to_string_lossy()
            .to_string(),
        None,
        None,
    );
    let route = NotifyRoute {
        channel: "qq".into(),
        target: "user:U1".into(),
        msg_id: Some("m1".into()),
    };

    let first = prep_video_task(
        &store,
        &client,
        "vid-1",
        "AI / 安全",
        "CET-6 / B2",
        24_000,
        "codex",
        Some(&route),
    )
    .await
    .unwrap();
    assert!(first.contains("预习任务已创建"), "{first}");
    let tasks = store.list(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].request_key, "youtube:prep:vid-1");
    assert_eq!(
        tasks[0].notify.as_ref().unwrap().msg_id.as_deref(),
        Some("m1")
    );

    // 同一视频重复提交：幂等返回既有任务。
    let again = prep_video_task(
        &store,
        &client,
        "vid-1",
        "AI / 安全",
        "CET-6 / B2",
        24_000,
        "codex",
        Some(&route),
    )
    .await
    .unwrap();
    assert!(again.contains("已有预习任务"), "{again}");
    assert_eq!(store.list(10).unwrap().len(), 1);
}

#[test]
fn rejects_unknown_config_fields() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.youtube]\nchannels = [\"@a\"]\nbogus_field = 1\n",
    )
    .unwrap();
    // 分布式配置：[sources] 层不再逐字段校验，但模块 Section 读取时
    // deny_unknown_fields 仍然兜住拼错的字段名。
    let (config, _) = ru41::bootstrap::config::Config::load_from(None, cwd.path(), None).unwrap();
    let result = config
        .sources
        .get::<ru41::modules::youtube::YoutubeSection>("youtube");
    assert!(result.is_err());
}

/// 复习闭环集成测试：真 Sqlite store + 假通知器。
mod review_loop {
    use ru41::{
        adapters::database::sqlite::SqliteTaskStore,
        app::task::TaskService,
        domain::{AgentResult, RunContract},
        modules::youtube::review::{self, YoutubeReviewJob},
        ports::{channel::NotifyChannel, storage::TaskStore},
    };
    use std::{future::Future, pin::Pin, sync::Arc};

    const SHEET: &str = "📺 测试视频主线\n\
🔗 https://www.youtube.com/watch?v=vidq\n⏱ 5:00\n\n\
📖 现场装备（⭐ 进间隔复习）：\n\
1. ⭐ **silent stalls** 助手卡住不回应；\"Okay. Silent stalls sounds like\" [2:44]\n\
2. **fall back** 首选不可用改用可用；\"Instead, it could just fall back\" [3:32]\n\n\
🔗 重点单词（认识但不会主动用的词）：\n\
1. **verbose** /vɜːˈboʊs/ adj. 冗长的；搭配 verbose output；词形 verbosity；\"it was way too verbose\" [1:12]\n\
\n\
🔁 带走就用（2-3 个可迁移口语块）：\n\
- **\"walk through … with you\"** 带人逐步看懂 → 你可以说：\"I'll walk through the code with you.\"；\"I'm going to walk through that with you.\" [2:02]\n\
\n\
🧩 重点句式（1-2 个）：\n\
- **\"Verb + sb + into + doing\"** 说服某人做某事 → 你可以说：\"He talked me into trying Rust.\"；\"gaslit himself into thinking\" [2:03]\n\n\
🎤 口语任务：用 try + V-ing 说一句你自己的真实情况。\n\
🗣 雅思口语迁移（45-60 秒连续表达）：AI 会取代专业工作吗？观点→原因→例子→结论。\n\
✍️ 雅思写作迁移（1-2 句）：把「AI 让律师更高效地审查协议」译成英文（用 allow sb to do）\n\
👂 精听挑战（每处①盲听写词②对照找漏听③跟读）：\n\
- [2:02] \"I'm going to walk through that with you.\" → going to 快读缩成 /ɡənə/（现象：缩读）\n\
- [3:12] \"better at improving\" → better 的 /t/ 变轻拍音（现象：闪音）\n\
- [3:40] \"and I put out a PR\" → and 弱读成 /ən/（现象：弱读）\n\n\
❓ 听前自测：测试问题？\n\
✅ 看后检测（看完再做）：\n\
1. 主线是什么？\n\
答案：1A 2 gear up\n";

    #[derive(Default, Clone)]
    struct FakeNotifier(Arc<std::sync::Mutex<Vec<String>>>);

    impl NotifyChannel for FakeNotifier {
        fn channel_id(&self) -> &str {
            "fake"
        }
        fn send<'a>(
            &'a self,
            _target: &'a str,
            _msg_id: Option<&'a str>,
            text: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
            let sink = self.0.clone();
            let text = text.to_owned();
            Box::pin(async move {
                sink.lock().unwrap().push(text);
                Ok(())
            })
        }
    }

    fn review_job(store: Arc<dyn TaskStore>) -> (YoutubeReviewJob, FakeNotifier) {
        review_job_at(store, chrono::Local::now())
    }

    fn review_job_at(
        store: Arc<dyn TaskStore>,
        at: chrono::DateTime<chrono::Local>,
    ) -> (YoutubeReviewJob, FakeNotifier) {
        let config = ru41::modules::youtube::YoutubeConfig {
            channels: vec![],
            keywords: vec![],
            interests: String::new(),
            level: "CET-6 / B2".into(),
            min_duration_secs: 60,
            max_per_channel: 15,
            max_per_poll: 2,
            max_age_days: 14,
            max_transcript_chars: 24_000,
            interval_secs: 21_600,
            backend: "codex".into(),
            notify: Some(ru41::domain::NotifyRoute {
                channel: "qq".into(),
                target: "user:U1".into(),
                msg_id: None,
            }),
            digest_at: "12:10".into(),
            ytdlp_bin: "yt-dlp".into(),
            proxy_url: None,
            ytdlp_cookies: None,
        };
        let fake = FakeNotifier::default();
        (
            YoutubeReviewJob::with_clock(
                store,
                config,
                Some(Arc::new(fake.clone())),
                Arc::new(move || at),
            ),
            fake,
        )
    }

    /// 今天/明天 12:15 的时钟时刻（与真实日历对齐，方便断言日期键）。
    fn clock_at(days_ahead: i64) -> chrono::DateTime<chrono::Local> {
        use chrono::{Duration, TimeZone};
        let date = (chrono::Local::now() + Duration::days(days_ahead)).date_naive();
        let naive = date.and_hms_opt(12, 15, 0).expect("valid time");
        chrono::Local
            .from_local_datetime(&naive)
            .single()
            .expect("local time")
    }

    fn seed_legacy_sheet(store: &Arc<dyn TaskStore>, video: &str) {
        // 旧格式预习单：没有「听力锚点」段（834ac93 之前的模板产物）。
        let mut service = TaskService::new(store.clone());
        let enqueue = service
            .enqueue_for(&format!("youtube:prep:{video}"), "prompt", "codex")
            .unwrap();
        let claim = store
            .claim_for(
                "test-worker",
                "codex",
                None,
                RunContract::AnalysisV1,
                ru41::util::now(),
                600,
            )
            .unwrap()
            .expect("task claimable");
        assert_eq!(claim.task.id, enqueue.task.id);
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: String::from(
                        "📺 旧格式单子\n🔗 https://www.youtube.com/watch?v=old\n⏱ 5:00\n\n                         📖 重点词句（8-12 个）：\n                         1. **footage** /ˈfʊtɪdʒ/ 拍摄的视频素材 — \"my footage from my GoPro\" [1:00]\n",
                    ),
                },
                None,
                ru41::util::now(),
            )
            .unwrap();
    }

    fn seed_succeeded_sheet(store: &Arc<dyn TaskStore>, video: &str) {
        let mut service = TaskService::new(store.clone());
        let enqueue = service
            .enqueue_for(&format!("youtube:prep:{video}"), "prompt", "codex")
            .unwrap();
        let claim = store
            .claim_for(
                "test-worker",
                "codex",
                None,
                RunContract::AnalysisV1,
                ru41::util::now(),
                600,
            )
            .unwrap()
            .expect("task claimable");
        assert_eq!(claim.task.id, enqueue.task.id);
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: SHEET.replace("vidq", video),
                },
                None,
                ru41::util::now(),
            )
            .unwrap();
    }

    fn seed_dead_sheet(store: &Arc<dyn TaskStore>, video: &str) {
        // 重做流程的产物：旧任务 request_key 加 :dead 后缀后重提交同一视频。
        let mut service = TaskService::new(store.clone());
        let enqueue = service
            .enqueue_for(&format!("youtube:prep:{video}:dead"), "prompt", "codex")
            .unwrap();
        let claim = store
            .claim_for(
                "test-worker",
                "codex",
                None,
                RunContract::AnalysisV1,
                ru41::util::now(),
                600,
            )
            .unwrap()
            .expect("task claimable");
        assert_eq!(claim.task.id, enqueue.task.id);
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: SHEET.replace("vidq", video) + "\n（作废旧单子标记）\n",
                },
                None,
                ru41::util::now(),
            )
            .unwrap();
    }

    fn vocab(store: &Arc<dyn TaskStore>, video: &str, index: usize) -> serde_json::Value {
        store
            .document("youtube_vocab", &format!("{video}#{index}"))
            .unwrap()
            .unwrap()
            .value
    }

    #[tokio::test]
    async fn extract_push_feedback_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        seed_succeeded_sheet(&store, "vidq");
        let day1 = clock_at(0);
        let (job, fake) = review_job_at(store.clone(), day1);

        // 第一天 12:15 日报：预习单进日报；词块全部明天才到期，无复习段。
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["extracted_videos"], 1);
        assert_eq!(report["digest_pushed"], 1);
        let meta = store
            .document("youtube_vocab", "vidq#meta")
            .unwrap()
            .unwrap();
        assert_eq!(meta.value["count"], 4);
        assert_eq!(vocab(&store, "vidq", 0)["chunk"], "silent stalls");
        assert_eq!(vocab(&store, "vidq", 1)["chunk"], "verbose");
        assert_eq!(vocab(&store, "vidq", 2)["chunk"], "walk through … with you");
        {
            let sent = fake.0.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(sent[0].contains("📚 英语日报"), "{}", sent[0]);
            assert!(sent[0].contains("silent stalls"), "日报应含预习单全文");
            assert!(!sent[0].contains("答案：1A"), "答案不得出现在日报正文");
            assert!(sent[0].contains("回复 /答案"), "应提示 /答案 获取方式");
            assert!(!sent[0].contains("词块复习"), "未到期不该有复习段");
        }

        // 同日再跑：日报已发，不重发。
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 0);
        assert_eq!(fake.0.lock().unwrap().len(), 1);

        // 回拨第 0 个词块的到期时间，模拟「明天该复习了」。
        let doc = store.document("youtube_vocab", "vidq#0").unwrap().unwrap();
        let mut value = doc.value.clone();
        value["due_at"] = (ru41::util::now() - 60).into();
        store
            .put_document("youtube_vocab", "vidq#0", Some(doc.revision), &value)
            .unwrap();

        // 第二天 12:15 日报：复习段只有到期的 1 个，批次登记在明天名下。
        let day2 = clock_at(1);
        let (job, fake) = review_job_at(store.clone(), day2);
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 1);
        {
            let sent = fake.0.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(sent[0].contains("词块复习"));
            assert!(sent[0].contains("（懂）助手卡住不回应"));
            assert!(
                !sent[0].contains("Silent stalls sounds like"),
                "线索泄漏英文原句"
            );
            assert!(!sent[0].contains("带人逐步看懂"), "未到期的不该出现在批里");
        }
        let day2_str = day2.format("%Y-%m-%d").to_string();
        let batch = store
            .document("youtube_review_batch", &day2_str)
            .unwrap()
            .unwrap();
        assert_eq!(batch.value["items"].as_array().unwrap().len(), 1);

        // 反馈：/复习 all 记会，进一档。
        let reply = review::apply_feedback(&store, "all").unwrap();
        assert!(reply.contains("都会"), "{}", reply);
        assert_eq!(vocab(&store, "vidq", 0)["stage"], 1);

        // 同日再跑：日报已发，不重发。
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 0);
        assert_eq!(fake.0.lock().unwrap().len(), 1);

        // 连「会」到毕业：known 词块进入排除名单。
        for _ in 0..4 {
            // 批次已 resolved，先重开它模拟跨天新批次，再反馈推进阶梯。
            let doc = store
                .document("youtube_review_batch", &day2_str)
                .unwrap()
                .unwrap();
            let mut value = doc.value.clone();
            value["resolved"] = false.into();
            value["pushed_at"] = ru41::util::now().into();
            store
                .put_document(
                    "youtube_review_batch",
                    &day2_str,
                    Some(doc.revision),
                    &value,
                )
                .unwrap();
            review::apply_feedback(&store, "all").unwrap();
        }
        assert_eq!(vocab(&store, "vidq", 0)["known"], true);
        let known = review::known_chunks(&store, 10);
        assert!(known.contains(&"silent stalls".to_string()));
    }

    #[tokio::test]
    async fn anchor_topic_push_after_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        // 3 张单子各贡献 1 条「缩读」锚点 → 达到专题阈值。
        for video in ["vidq", "vidr", "vids"] {
            seed_succeeded_sheet(&store, video);
        }
        let (job, fake) = review_job_at(store.clone(), clock_at(0));
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 1, "日报打包：预习单");
        {
            let sent = fake.0.lock().unwrap();
            assert_eq!(sent.len(), 1, "一天只有一条日报");
            assert!(sent[0].contains("📚 英语日报"), "{}", sent[0]);
            assert!(
                !sent[0].contains("发音专题"),
                "专题不得带当天精听刚覆盖的实例：{}",
                sent[0]
            );
            assert!(
                [
                    "连读", "弱读", "闪音", "失爆", "同化", "缩读", "重音", "其他"
                ]
                .iter()
                .any(|c| sent[0].contains(c)),
                "专题应带现象类别：{}",
                sent[0]
            );
            assert!(
                ["[2:02]", "[3:12]", "[3:40]"]
                    .iter()
                    .any(|ts| sent[0].contains(ts)),
                "例句应带时间戳：{}",
                sent[0]
            );
        }
        // 第二天：单子上限 2/天，vids 滞留进当日日报 → 其锚点仍被当日排除；
        // 前两天的锚点隔夜可用但只有 2 条，未达阈值。
        let (job2, fake2) = review_job_at(store.clone(), clock_at(1));
        let report = ru41::ingress::schedule::PollJob::poll_json(&job2)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 1, "滞留单子单独成日报");
        {
            let sent = fake2.0.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(
                !sent[0].contains("发音专题"),
                "第二天只有 2 条隔夜锚点，未达阈值：{}",
                sent[0]
            );
        }
        // 第三天：三张单子都已推完，无当日排除 → 缩读 3 条凑齐阈值成专题。
        let (job3, fake3) = review_job_at(store.clone(), clock_at(2));
        let report = ru41::ingress::schedule::PollJob::poll_json(&job3)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 1, "专题单独成日报也算有内容");
        {
            let sent = fake3.0.lock().unwrap();
            assert!(sent[0].contains("发音专题"), "第三天应推专题：{}", sent[0]);
            assert!(
                [
                    "连读", "弱读", "闪音", "失爆", "同化", "缩读", "重音", "其他"
                ]
                .iter()
                .any(|c| sent[0].contains(c)),
                "专题应带现象类别：{}",
                sent[0]
            );
            assert!(
                ["[2:02]", "[3:12]", "[3:40]"]
                    .iter()
                    .any(|ts| sent[0].contains(ts)),
                "例句应带时间戳：{}",
                sent[0]
            );
        }
        // 锚点已标记 topic_pushed，日报当天已发 → 再跑不重发。
        let report = ru41::ingress::schedule::PollJob::poll_json(&job3)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 0);
        assert_eq!(fake3.0.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn latest_sheet_returns_newest_full_sheet() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        assert!(
            review::latest_sheet(&store)
                .unwrap()
                .contains("最近没有预习单")
        );
        seed_succeeded_sheet(&store, "vidq");
        let sheet = review::latest_sheet(&store).unwrap();
        assert!(sheet.contains("听前自测"), "完整单子应含自测");
        assert!(sheet.contains("答案：1A"), "完整单子应含检测答案");
    }

    #[tokio::test]
    async fn digest_excludes_dead_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        seed_succeeded_sheet(&store, "vidq");
        seed_dead_sheet(&store, "vidq");
        let (job, fake) = review_job_at(store.clone(), clock_at(0));

        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 1);
        assert_eq!(report["extracted_videos"], 1, "dead 任务不参与提取");
        let sent = fake.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(
            !sent[0].contains("作废旧单子标记"),
            "dead 任务不得进日报: {}",
            sent[0]
        );
    }

    #[tokio::test]
    async fn legacy_format_sheets_never_enter_digest() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        seed_legacy_sheet(&store, "oldvid");
        let (job, fake) = review_job_at(store.clone(), clock_at(0));
        let report = ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        assert_eq!(report["digest_pushed"], 0, "旧格式单子不得进日报");
        assert!(fake.0.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn latest_answers_returns_newest_sheet_answer() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        seed_succeeded_sheet(&store, "vidq");
        let reply = review::latest_answers(&store).unwrap();
        assert!(reply.contains("答案：1A 2 gear up"), "{}", reply);

        let dir2 = tempfile::tempdir().unwrap();
        let empty: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir2.path().join("state.db")).unwrap());
        assert!(review::latest_answers(&empty).unwrap().contains("没有"));
    }

    #[test]
    fn extracts_without_english_leak() {
        let items = review::extract_vocab(SHEET);
        assert_eq!(items.len(), 4);
        assert!(items.iter().all(|item| item.chunk != "fall back"));
        for item in &items {
            assert!(!item.cue.contains("sounds like"));
            assert!(!item.cue.contains("你可以说"));
        }
    }

    #[tokio::test]
    async fn review_batch_mixes_videos() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        seed_succeeded_sheet(&store, "vidq");
        seed_succeeded_sheet(&store, "vidr");
        let (job, _fake) = review_job_at(store.clone(), clock_at(0));
        // 第一天日报：带走两张预习单，复习段为空。
        ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();

        // 两个视频全部 6 个词块都到期。
        for video in ["vidq", "vidr"] {
            for index in 0..3 {
                let doc = store
                    .document("youtube_vocab", &format!("{video}#{index}"))
                    .unwrap()
                    .unwrap();
                let mut value = doc.value.clone();
                value["due_at"] = (ru41::util::now() - 60).into();
                store
                    .put_document(
                        "youtube_vocab",
                        &format!("{video}#{index}"),
                        Some(doc.revision),
                        &value,
                    )
                    .unwrap();
            }
        }
        // 第二天日报：复习段 2 视频 × 单视频上限 2 = 4 条。
        let (job, fake) = review_job_at(store.clone(), clock_at(1));
        ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        {
            let sent = fake.0.lock().unwrap();
            assert_eq!(sent.len(), 1);
            assert!(sent[0].contains("词块复习 4 条"), "{}", sent[0]);
            assert!(sent[0].contains("（懂）"), "{}", sent[0]);
        }
        let day2 = clock_at(1).format("%Y-%m-%d").to_string();
        let batch = store
            .document("youtube_review_batch", &day2)
            .unwrap()
            .unwrap();
        let items = batch.value["items"].as_array().unwrap();
        assert_eq!(items.len(), 4, "2 视频 × 单视频上限 2");
        let q_count = items
            .iter()
            .filter(|v| v.as_str().unwrap().starts_with("vidq"))
            .count();
        assert_eq!(q_count, 2, "单视频不超过 MAX_PER_VIDEO");
    }

    #[tokio::test]
    async fn placement_flow_updates_level() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        let now = ru41::util::now();

        // 出题。
        let reply = review::place_start(&store, "codex", None).await.unwrap();
        assert!(reply.contains("稍后推送"), "{reply}");
        let place = store
            .list(10)
            .unwrap()
            .into_iter()
            .find(|t| t.request_key.starts_with("youtube:place:"))
            .expect("placement task enqueued");
        let claim = store
            .claim_for("w", "codex", None, RunContract::AnalysisV1, now, 600)
            .unwrap()
            .unwrap();
        assert_eq!(claim.task.id, place.id);
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: "📏 定级测试｜1. She is late. 2. The agent keeps building on it. 3. Had it not been for the scoring function, the loop would start over.".into(),
                },
                None,
                now,
            )
            .unwrap();

        // 交卷 → 评测任务入队。
        let reply = review::place_submit(
            &store,
            "1. 她迟到了 2. 智能体继续改进 3. 如果没有评分函数",
            "codex",
            None,
        )
        .await
        .unwrap();
        assert!(reply.contains("评测"), "{reply}");
        let eval = store
            .list(10)
            .unwrap()
            .into_iter()
            .find(|t| t.request_key.starts_with("youtube:place-eval:"))
            .expect("eval task enqueued");
        assert!(eval.prompt.contains("start over"), "评测 prompt 应带原题");
        let claim = store
            .claim_for("w", "codex", None, RunContract::AnalysisV1, now, 600)
            .unwrap()
            .unwrap();
        assert_eq!(claim.task.id, eval.id);
        store
            .succeed(
                &claim,
                &AgentResult {
                    summary: "🔊 定级：B2+\n\n1. 对。2. 偏差：building on 是在现有基础上改进。3. 虚拟语气没译出来；这个级别最该练长难句。".into(),
                },
                None,
                now,
            )
            .unwrap();

        // 复习 job 回收级别 → youtube_level.latest；预习级别随之更新。
        let (job, _fake) = review_job(store.clone());
        ru41::ingress::schedule::PollJob::poll_json(&job)
            .await
            .unwrap();
        let level_doc = store
            .document("youtube_level", "latest")
            .unwrap()
            .expect("level record");
        assert_eq!(level_doc.value["level"], "B2+");
        assert_eq!(level_doc.value["task_id"], eval.id);
        assert_eq!(review::current_level(&store, "CET-6 / B2"), "B2+");

        // 已交卷后再交 → 引导重新测试。
        let reply = review::place_submit(&store, "再交一次", "codex", None)
            .await
            .unwrap();
        assert!(reply.contains("/定级"), "{reply}");
    }
}
