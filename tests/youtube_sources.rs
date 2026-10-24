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
fn write_fake_ytdlp(dir: &Path, marker: &Path, list_lines: &str) -> PathBuf {
    let script = dir.join("fake-ytdlp.sh");
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
        ytdlp_bin: write_fake_ytdlp(dir, marker, list_lines)
            .to_string_lossy()
            .to_string(),
        proxy_url: None,
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
    assert_eq!(tasks[0].notify.as_ref().unwrap().target, "user:U1");
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
    let result = ru41::bootstrap::config::Config::load_from(None, cwd.path(), None);
    assert!(result.is_err());
}
