use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{
        NotifyRoute,
        todo::{TodoItem, TodoPriority, TodoStatus},
    },
    modules::zhihuishu::{ZhihuishuConfig, ZhihuishuSection, ZhihuishuTracker, load_report},
    ports::{channel::NotifyChannel, storage::TaskStore},
};
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

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

fn platform_time(offset_days: i64) -> String {
    use chrono::Datelike;
    let dt = chrono::Local::now() + chrono::Duration::days(offset_days);
    format!(
        "{:04}-{:02}-{:02} 23:59:00",
        dt.year(),
        dt.month(),
        dt.day()
    )
}

fn sample_report_json() -> serde_json::Value {
    json!({
        "generated_at": "2026-10-19 15:00:00",
        "login_ok": true,
        "courses": [
            {
                "title": "职业生涯规划",
                "platform": "polymas",
                "course_home_url": "https://hike-teaching-center.polymas.com/x/stu/course-home",
                "content_published": true,
                "tasks": [
                    {
                        "activity_task_id": "T1",
                        "title": "第五节课 课前测",
                        "kind": "assignment",
                        "start_at": platform_time(-1),
                        "end_at": platform_time(1),
                        "completed": false,
                        "total_score": 20.0,
                        "exam_duration_minutes": null,
                        "video_invigilate": null,
                        "late_submit": false,
                        "content": "设置三个习题，用于回顾第四节的知识点，请认真完成。",
                        "attachments": [
                            {"name": "第五节课 课前测说明.docx", "file": "attachments/T1/说明.docx"}
                        ],
                    },
                    {
                        "activity_task_id": "T2",
                        "title": "形势与政策作业",
                        "kind": "exam",
                        "end_at": platform_time(6),
                        "completed": false,
                        "exam_duration_minutes": 30,
                        "video_invigilate": true
                    },
                    {
                        "activity_task_id": "T3",
                        "title": "远期作业",
                        "kind": "assignment",
                        "end_at": platform_time(30),
                        "completed": false
                    },
                    {
                        "activity_task_id": "T4",
                        "title": "已完成作业",
                        "kind": "assignment",
                        "end_at": platform_time(-2),
                        "completed": true
                    }
                ]
            }
        ]
    })
}

fn write_report(dir: &std::path::Path, value: &serde_json::Value) -> std::path::PathBuf {
    let path = dir.to_path_buf().join("report.json");
    std::fs::write(&path, serde_json::to_string_pretty(value).unwrap()).unwrap();
    path
}

fn make_tracker(
    store: &Arc<dyn TaskStore>,
    report_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
) -> ZhihuishuTracker {
    make_tracker_with(store, report_path, notifier, true, None)
}

fn make_tracker_with(
    store: &Arc<dyn TaskStore>,
    report_path: std::path::PathBuf,
    notifier: Option<Arc<dyn NotifyChannel>>,
    todo_sync: bool,
    vault: Option<Arc<dyn ru41::ports::vault::Vault>>,
) -> ZhihuishuTracker {
    ZhihuishuTracker::new(
        store.clone(),
        notifier,
        ZhihuishuConfig {
            attachments_dir: report_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| ".".into()),
            report_path,
            deadline_days: 3,
            interval_secs: 21_600,
            todo_sync,
            vault_sync: true,
            vault_course_dir: "01-Courses-课程/2026-Fall-大一上".into(),
            vault_courses: [("职业生涯规划".to_owned(), "大学生职业生涯规划".to_owned())]
                .into_iter()
                .collect(),
            vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
            notify_target: "user:mock".into(),
        },
        ru41::domain::todo::TodoOptions {
            offset_seconds: 28_800,
            recent_days: 7,
            long_term_days: 30,
        },
        vault,
    )
}

/// 内存版 Vault：write_file 内容变化才置 dirty，commit_all 消费 dirty（模拟 git 空提交）。
#[derive(Default)]
struct FakeVault {
    files: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
    dirty: std::sync::Mutex<bool>,
    fail_writes: std::sync::Mutex<bool>,
}

impl ru41::ports::vault::Vault for FakeVault {
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
    fn checkout_hard(&self, _sha: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn commit_all(&self, _message: &str) -> anyhow::Result<bool> {
        let mut dirty = self.dirty.lock().unwrap();
        Ok(std::mem::take(&mut *dirty))
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
        if *self.fail_writes.lock().unwrap() {
            anyhow::bail!("simulated vault write failure");
        }
        let mut files = self.files.lock().unwrap();
        if files.get(relative).map(String::as_str) != Some(content) {
            *self.dirty.lock().unwrap() = true;
        }
        files.insert(relative.to_owned(), content.to_owned());
        Ok(())
    }
    fn remove_file(&self, relative: &str) -> anyhow::Result<bool> {
        Ok(self.files.lock().unwrap().remove(relative).is_some())
    }
    fn write_bytes(&self, relative: &str, data: &[u8]) -> anyhow::Result<()> {
        if *self.fail_writes.lock().unwrap() {
            anyhow::bail!("simulated vault write failure");
        }
        let mut files = self.files.lock().unwrap();
        let content = String::from_utf8_lossy(data).to_string();
        if files.get(relative).map(String::as_str) != Some(&content) {
            *self.dirty.lock().unwrap() = true;
        }
        files.insert(relative.to_owned(), content);
        Ok(())
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

fn make_store(dir: &std::path::Path) -> Arc<dyn TaskStore> {
    Arc::new(SqliteTaskStore::open(&dir.join("state.db")).unwrap())
}

#[test]
fn parses_report_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_report(dir.path(), &sample_report_json());
    let report = load_report(&path).unwrap();
    assert_eq!(report.courses.len(), 1);
    let course = &report.courses[0];
    assert_eq!(course.title, "职业生涯规划");
    assert_eq!(course.tasks.len(), 4);
    let exam = &course.tasks[1];
    assert_eq!(exam.exam_duration_minutes, Some(30));
    assert_eq!(exam.video_invigilate, Some(true));
    assert_eq!(report.login_ok, Some(true));
}

#[test]
fn rejects_malformed_report() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("report.json");
    std::fs::write(&path, "{not json").unwrap();
    assert!(load_report(&path).is_err());
    // 缺 tasks/login 等可选字段时应能解析（全部有默认值）。
    let minimal = json!({"courses": [{"title": "军事理论", "activity_task_id": "x"}]});
    let path = write_report(dir.path(), &minimal);
    // activity_task_id 属于 task 层而非 course 层，这里 tasks 缺省为空。
    let report = load_report(&path).unwrap();
    assert_eq!(report.courses[0].tasks.len(), 0);
}

#[tokio::test]
async fn notifies_new_tasks_once_then_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker(&store, path.clone(), Some(channel.clone()));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.courses, 1);
    assert_eq!(report.tasks, 4);
    assert_eq!(report.new_tasks, 4);
    assert!(report.notified);
    // 新任务提醒（首个任务临近截止，每日提醒门当天也会放行，但仍合并为一条消息）。
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
    let first = channel.sent.lock().unwrap()[0].clone();
    assert!(first.starts_with("user:mock|"));
    assert!(first.contains("新作业"));
    assert!(first.contains("考试式·需人工"));
    // 临近截止与考试式进入分类列表。
    assert_eq!(report.due_soon.len(), 1);
    assert_eq!(report.exam_style.len(), 1);
    assert!(report.overdue.is_empty());

    // 第二轮：无新任务，每日提醒已入库，不再发送。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_tasks, 0);
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn content_published_announced_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker(&store, path.clone(), Some(channel.clone()));

    // 首轮：内容已发布的课程进入播报与摘要。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.content_ready, vec!["职业生涯规划".to_owned()]);
    let first = channel.sent.lock().unwrap()[0].clone();
    assert!(first.contains("视频内容已发布"));

    // 次轮：同一课程不重复播报。
    let report = tracker.poll_once().await.unwrap();
    assert!(report.content_ready.is_empty());
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn content_unpublished_and_unknown_never_announce() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let mut value = sample_report_json();
    value["courses"][0]["content_published"] = json!(false);
    let path = write_report(dir.path(), &value);
    let tracker = make_tracker(&store, path, None);

    let report = tracker.poll_once().await.unwrap();
    assert!(report.content_ready.is_empty());

    // 字段缺失(None, wenda/图谱版)同样静默。
    let dir2 = tempfile::tempdir().unwrap();
    let store2 = make_store(dir2.path());
    let mut value2 = sample_report_json();
    value2["courses"][0]
        .as_object_mut()
        .unwrap()
        .remove("content_published");
    let path2 = write_report(dir2.path(), &value2);
    let tracker2 = make_tracker(&store2, path2, None);
    let report2 = tracker2.poll_once().await.unwrap();
    assert!(report2.content_ready.is_empty());
}

#[tokio::test]
async fn todo_created_then_synced_on_platform_completion() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker(&store, path.clone(), Some(channel.clone()));

    // 首轮：未完成任务自动进待办（已完成的历史任务不建）。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_tasks, 4);
    let todo_of = |key: &str| {
        let snap = store.document("zhihuishu_task", key).unwrap().unwrap();
        let id = snap.value["todo_id"].as_str().unwrap().to_owned();
        let doc = store.document("todo_item", &id).unwrap().unwrap();
        let item: TodoItem = serde_json::from_value(doc.value).unwrap();
        item
    };
    let t1 = todo_of("职业生涯规划#T1");
    assert!(t1.title.contains("智慧树｜职业生涯规划｜第五节课 课前测"));
    assert!(t1.due_at.is_some() && t1.remind_at.is_some());
    assert_eq!(t1.source.as_ref().unwrap().channel, "zhihuishu");
    assert!(channel.sent.lock().unwrap()[0].contains("已入待办"));
    // 考试式任务：🔒 前缀 + High 优先级。
    let t2 = todo_of("职业生涯规划#T2");
    assert!(t2.title.starts_with("🔒"));
    assert_eq!(t2.priority, TodoPriority::High);

    // 次轮不重复创建（样本 4 任务中 3 个未完成 → 恰 3 条待办）。
    tracker.poll_once().await.unwrap();
    assert_eq!(store.documents("todo_item", 100).unwrap().len(), 3);

    // 平台侧完成后：待办被回勾，且只回勾一次。
    let mut done = sample_report_json();
    done["courses"][0]["tasks"][0]["completed"] = json!(true);
    write_report(dir.path(), &done);
    tracker.poll_once().await.unwrap();
    let t1_after = todo_of("职业生涯规划#T1");
    assert_eq!(t1_after.status, TodoStatus::Completed);
    assert!(t1_after.completed_at.is_some());
    let last = channel.sent.lock().unwrap().last().unwrap().clone();
    assert!(last.contains("平台已完成，待办已勾选"));

    // 再来一轮：todo_synced 已入库，不重复回勾、不重复播报。
    tracker.poll_once().await.unwrap();
    let total: usize = channel
        .sent
        .lock()
        .unwrap()
        .iter()
        .filter(|m| m.contains("平台已完成"))
        .count();
    assert_eq!(total, 1);
    // 回归：快照的完成标记必须真的落库（put_document(None) 仅新建会静默丢写），
    // 否则作业总表永远把它列为未完成、每轮重复调 complete。
    let snap = store
        .document("zhihuishu_task", "职业生涯规划#T1")
        .unwrap()
        .unwrap();
    assert_eq!(snap.value["todo_synced"], json!(true));
    assert_eq!(snap.value["completed"], json!(true));
}

#[tokio::test]
async fn todo_sync_disabled_creates_no_todos() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker_with(&store, path, None, false, None);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_tasks, 4);
    assert!(store.documents("todo_item", 100).unwrap().is_empty());
    let snap = store
        .document("zhihuishu_task", "职业生涯规划#T1")
        .unwrap()
        .unwrap();
    assert!(snap.value.get("todo_id").is_none());
}

#[tokio::test]
async fn vault_syncs_course_index_and_task_notes() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let vault = Arc::new(FakeVault::default());
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker_with(
        &store,
        path,
        Some(channel.clone()),
        true,
        Some(vault.clone()),
    );

    // sidecar 附件落盘(报告里 T1 声明了它)
    let att_dir = dir.path().join("attachments/T1");
    std::fs::create_dir_all(&att_dir).unwrap();
    std::fs::write(att_dir.join("说明.docx"), b"fake docx bytes").unwrap();

    // 首轮：课程索引 + 4 份作业说明 + 1 个附件 = 6 个文件
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.vault_files, 6);
    assert!(channel.sent.lock().unwrap()[0].contains("已同步到 Obsidian"));

    // 作用域块：guard 在 await 前释放（clippy::await_holding_lock）
    {
        let files = vault.files.lock().unwrap();
        // 课程名映射生效：职业生涯规划 → 大学生职业生涯规划
        let index_key =
            "01-Courses-课程/2026-Fall-大一上/大学生职业生涯规划/智慧树作业.md".to_owned();
        let index = files.get(&index_key).unwrap();
        assert!(index.contains("未完成（3）"), "T4 已完成不入未完成组");
        // 任务行双链到「日期在前」的文件夹（T1 布置于昨天）
        let yst = (chrono::Local::now() - chrono::Duration::days(1)).format("%Y.%m.%d");
        assert!(
            index.contains(&format!("[[{yst}_第五节课 课前测/作业说明|")),
            "索引应双链到日期前缀文件夹"
        );
        // 作业说明：描述文本 + 元信息
        let task_key = format!(
            "01-Courses-课程/2026-Fall-大一上/大学生职业生涯规划/{yst}_第五节课 课前测/作业说明.md"
        );
        let note = files.get(&task_key).unwrap();
        assert!(note.contains("设置三个习题"), "平台描述应进入作业说明");
        assert!(note.contains("截止：") && note.contains("分值：20"));
        // 平台附件按原名拷进同一作业文件夹
        let att_key = format!(
            "01-Courses-课程/2026-Fall-大一上/大学生职业生涯规划/{yst}_第五节课 课前测/第五节课 课前测说明.docx"
        );
        assert_eq!(
            files.get(&att_key).map(String::as_str),
            Some("fake docx bytes")
        );
    }

    // 第二轮内容无变化 → 不提交不播报
    let report2 = tracker.poll_once().await.unwrap();
    assert_eq!(report2.vault_files, 0);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn vault_failure_degrades_to_warning() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let vault = Arc::new(FakeVault {
        fail_writes: std::sync::Mutex::new(true),
        ..Default::default()
    });
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker_with(&store, path, Some(channel.clone()), true, Some(vault));

    // 写失败不影响轮询本身：任务照常入快照、待办照建，仅一条降级提示
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_tasks, 4);
    assert_eq!(report.vault_files, 0);
    let sent = channel.sent.lock().unwrap();
    assert!(sent[0].contains("Obsidian 同步失败"));
}

#[tokio::test]
async fn overdue_task_reported() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let mut report_value = sample_report_json();
    report_value["courses"][0]["tasks"] = json!([
        {
            "activity_task_id": "OLD1",
            "title": "过期的思维导图",
            "kind": "assignment",
            "end_at": platform_time(-1),
            "completed": false
        }
    ]);
    let path = write_report(dir.path(), &report_value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.overdue.len(), 1);
    assert!(report.notified);
    assert!(channel.sent.lock().unwrap()[0].contains("已过期"));
    assert!(channel.sent.lock().unwrap()[0].contains("过期的思维导图"));
}

#[tokio::test]
async fn completed_task_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let mut report_value = sample_report_json();
    report_value["courses"][0]["tasks"] = json!([
        {
            "activity_task_id": "DONE1",
            "title": "已提交的作业",
            "kind": "assignment",
            "end_at": platform_time(1),
            "completed": true
        }
    ]);
    // 本用例只验证任务静默, 显式关掉内容上线信号。
    report_value["courses"][0]["content_published"] = json!(false);
    let path = write_report(dir.path(), &report_value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.tasks, 1);
    assert_eq!(report.new_tasks, 1);
    assert!(report.due_soon.is_empty());
    // 已完成任务不触发任何提醒。
    assert!(!report.notified);
    assert!(channel.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn login_failure_alerts_once_per_day() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let mut report_value = sample_report_json();
    report_value["courses"] = json!([]);
    report_value["login_ok"] = json!(false);
    let path = write_report(dir.path(), &report_value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.login_ok, Some(false));
    assert!(report.notified);
    assert!(channel.sent.lock().unwrap()[0].contains("登录态失效"));

    // 同日第二轮不再重复提醒。
    let report = tracker.poll_once().await.unwrap();
    assert!(!report.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn missing_report_fails_single_round() {
    // 来源失败：报告文件缺失只影响本轮（poll 报错），不破坏 records 里的既有状态。
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let tracker = make_tracker(&store, dir.path().join("nope.json"), None);
    assert!(tracker.poll_once().await.is_err());
    // 已落库的任务快照不受影响（首轮失败时为空，仅为展示读法）。
    assert!(
        store
            .document("zhihuishu_task", "职业生涯规划#T1")
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn empty_task_list_is_silent() {
    // 无候选：课程在、任务列表为空 → 成功一轮但不产生任何提醒。
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let mut report_value = sample_report_json();
    report_value["courses"][0]["tasks"] = json!([]);
    // 本用例只验证任务静默, 显式关掉内容上线信号。
    report_value["courses"][0]["content_published"] = json!(false);
    let path = write_report(dir.path(), &report_value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.courses, 1);
    assert_eq!(report.tasks, 0);
    assert_eq!(report.new_tasks, 0);
    assert!(!report.notified);
    assert!(channel.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn works_without_notifier() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = make_tracker(&store, path, None);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.new_tasks, 4);
    assert!(!report.notified);
    // 无通知渠道时，每日提醒门不再写告警记录，records 只留任务快照。
    let seen = store.document("zhihuishu_task", "职业生涯规划#T1").unwrap();
    assert!(seen.is_some());
}

#[test]
fn config_section_rejects_unknown_fields() {
    assert!(toml::from_str::<ZhihuishuSection>("foo = 1").is_err());
    let section = toml::from_str::<ZhihuishuSection>(
        "report_path = \".ru41/zhihuishu/report.json\"\ndeadline_days = 2\nnotify = true",
    )
    .unwrap();
    assert_eq!(section.deadline_days, Some(2));
    assert!(section.notify.unwrap());
}

// NotifyRoute 引用保持与其它模块测试一致的导入面。
#[allow(dead_code)]
fn _route_type_check(_r: NotifyRoute) {}

// ---------- QQ「作业」命令 ----------

use ru41::app::identity::IdentityService;
use ru41::modules::zhihuishu::ZhihuishuQqCommand;
use ru41::ports::commands::{CommandContext, CommandHandler};

struct CtxFixture {
    _dir: tempfile::TempDir,
    store: Arc<dyn TaskStore>,
    target: String,
    author: String,
}

fn make_ctx_fixture() -> CtxFixture {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    CtxFixture {
        _dir: dir,
        store,
        target: "user:openid-1".into(),
        author: "openid-1".into(),
    }
}

fn ctx<'a>(f: &'a CtxFixture, c2c: bool) -> CommandContext<'a> {
    CommandContext {
        store: &f.store,
        bind_secret: Some("s3cret"),
        target: &f.target,
        msg_id: "msg-1",
        author_openid: &f.author,
        is_c2c: c2c,
        reply_channel: "qq",
        image_url: None,
    }
}

fn bind_user(f: &CtxFixture) {
    IdentityService::new(f.store.clone())
        .bind("qq", &f.author, "s3cret", "s3cret", 0)
        .unwrap();
}

#[test]
fn qq_matches_aliases_only() {
    let handler = ZhihuishuQqCommand::new(std::path::PathBuf::from("/nonexistent.json"));
    assert!(handler.matches("作业"));
    assert!(handler.matches("homework"));
    assert!(!handler.matches("tasks"));
    assert!(!handler.matches("水电"));
}

#[tokio::test]
async fn qq_arg_falls_through_to_agent() {
    let f = make_ctx_fixture();
    bind_user(&f);
    let handler = ZhihuishuQqCommand::new(f._dir.path().join("report.json"));
    let reply = handler.handle("帮我写作业", &ctx(&f, true)).await.unwrap();
    assert!(reply.is_none(), "带参数不拦截, 落给 Agent");
}

#[tokio::test]
async fn qq_unbound_user_is_prompted_to_bind() {
    let f = make_ctx_fixture();
    let handler = ZhihuishuQqCommand::new(f._dir.path().join("report.json"));
    let reply = handler.handle("", &ctx(&f, true)).await.unwrap().unwrap();
    assert!(reply.text.contains("绑定"));
}

#[tokio::test]
async fn qq_group_chat_is_rejected() {
    let f = make_ctx_fixture();
    bind_user(&f);
    let handler = ZhihuishuQqCommand::new(f._dir.path().join("report.json"));
    let reply = handler.handle("", &ctx(&f, false)).await.unwrap().unwrap();
    assert!(reply.text.contains("单聊"));
}

#[tokio::test]
async fn qq_lists_homework_for_bound_c2c() {
    let f = make_ctx_fixture();
    bind_user(&f);
    let mut report_value = sample_report_json();
    report_value["courses"][0]["tasks"] = json!([
        {
            "activity_task_id": "OLD1",
            "title": "过期的思维导图",
            "kind": "assignment",
            "end_at": platform_time(-1),
            "completed": false
        },
        {
            "activity_task_id": "SOON1",
            "title": "第五节课 课前测",
            "kind": "assignment",
            "end_at": platform_time(1),
            "completed": false
        },
        {
            "activity_task_id": "EXAM1",
            "title": "形势与政策 课程作业",
            "kind": "exam",
            "end_at": platform_time(6),
            "completed": false,
            "exam_duration_minutes": 60,
            "video_invigilate": true
        },
        {
            "activity_task_id": "DONE1",
            "title": "已完成作业",
            "kind": "assignment",
            "end_at": platform_time(-2),
            "completed": true
        }
    ]);
    let path = write_report(f._dir.path(), &report_value);
    let handler = ZhihuishuQqCommand::new(path);
    let reply = handler.handle("", &ctx(&f, true)).await.unwrap().unwrap();
    assert!(reply.text.contains("作业清单"));
    assert!(reply.text.contains("已过期") && reply.text.contains("过期的思维导图"));
    assert!(reply.text.contains("3 天内截止") && reply.text.contains("第五节课 课前测"));
    assert!(reply.text.contains("🔒"), "考试式任务需人工标注");
    assert!(!reply.text.contains("已完成作业"), "已完成任务不出现");
}

#[tokio::test]
async fn qq_natural_language_gate() {
    let f = make_ctx_fixture();
    bind_user(&f);
    write_report(f._dir.path(), &sample_report_json());
    let handler = ZhihuishuQqCommand::new(f._dir.path().join("report.json"));
    let miss = handler
        .handle_message("你好呀", &ctx(&f, true))
        .await
        .unwrap();
    assert!(miss.is_none(), "无关寒暄不拦截");
    let hit = handler
        .handle_message("作业还有哪些没交", &ctx(&f, true))
        .await
        .unwrap();
    assert!(hit.is_some(), "作业疑问应拦截并作答");
    assert!(hit.unwrap().text.contains("作业清单"));
}

#[tokio::test]
async fn qq_missing_report_degrades_friendly() {
    let f = make_ctx_fixture();
    bind_user(&f);
    let handler = ZhihuishuQqCommand::new(f._dir.path().join("nope.json"));
    let reply = handler.handle("", &ctx(&f, true)).await.unwrap().unwrap();
    assert!(reply.text.contains("暂不可用"));
}
