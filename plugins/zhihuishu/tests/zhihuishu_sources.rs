use ru41::{
    adapters::database::sqlite::SqliteTaskStore,
    domain::{
        NotifyRoute,
        todo::{TodoItem, TodoPriority, TodoStatus},
    },
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
            ignore_tasks: vec![],
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
    fn remote_branch_sha(&self, _branch: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
    fn push_head_to(&self, _remote_ref: &str, _force: bool) -> anyhow::Result<()> {
        Ok(())
    }
    fn delete_remote_branch(&self, _branch: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn head_sha(&self) -> anyhow::Result<String> {
        Ok("0".repeat(40))
    }
    fn merge_commit(&self, _sha: &str) -> anyhow::Result<ru41::ports::vault::SyncOutcome> {
        Ok(ru41::ports::vault::SyncOutcome::Clean)
    }
    fn merge_continue(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn merge_abort(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn conflict_files(&self) -> anyhow::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn is_ancestor(&self, _ancestor: &str, _other: &str) -> anyhow::Result<bool> {
        Ok(true)
    }

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
    fn list_files(&self, _at: &str, prefix: &str) -> anyhow::Result<Vec<String>> {
        Ok(self
            .files
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
    fn read_file_at(&self, _at: &str, relative: &str) -> anyhow::Result<Option<String>> {
        Ok(self.files.lock().unwrap().get(relative).cloned())
    }
    fn commit_all(&self, _message: &str) -> anyhow::Result<bool> {
        let mut dirty = self.dirty.lock().unwrap();
        Ok(std::mem::take(&mut *dirty))
    }
    fn push(&self) -> anyhow::Result<()> {
        Ok(())
    }
    fn sync(&self) -> anyhow::Result<ru41::ports::vault::SyncOutcome> {
        Ok(ru41::ports::vault::SyncOutcome::Clean)
    }
    fn resolve_conflict(&self, _relative: &str, _merged: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn rebase_continue(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn rebase_abort(&self) -> anyhow::Result<()> {
        Ok(())
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
    fn recent_inbox_moves(
        &self,
        _inbox_dir: &str,
        _days: u32,
    ) -> anyhow::Result<Vec<ru41::ports::vault::InboxMove>> {
        Ok(Vec::new())
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
    // 展示名走 vault 课程映射：平台原名 → 课表短名（目录名）。
    assert!(
        t1.title
            .contains("智慧树｜大学生职业生涯规划｜第五节课 课前测")
    );
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
    // 同步成果不进 QQ 推送（噪音），只有任务动态才推送
    assert!(!channel.sent.lock().unwrap()[0].contains("已同步到 Obsidian"));

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
async fn ignored_task_is_fully_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let vault = Arc::new(FakeVault::default());
    let path = write_report(dir.path(), &sample_report_json());
    let tracker = ZhihuishuTracker::new(
        store.clone(),
        Some(channel.clone()),
        ZhihuishuConfig {
            attachments_dir: path.parent().map(|p| p.to_path_buf()).unwrap(),
            report_path: path,
            deadline_days: 3,
            interval_secs: 21_600,
            todo_sync: true,
            vault_sync: true,
            vault_course_dir: "01-Courses-课程/2026-Fall-大一上".into(),
            vault_courses: [("职业生涯规划".to_owned(), "大学生职业生涯规划".to_owned())]
                .into_iter()
                .collect(),
            vault_task_dirs: ru41::ports::vault::VaultTaskDirs::All,
            ignore_tasks: vec!["职业生涯规划｜课前测".into()],
            notify_target: "user:mock".into(),
        },
        ru41::domain::todo::TodoOptions {
            offset_seconds: 28_800,
            recent_days: 7,
            long_term_days: 30,
        },
        Some(vault.clone()),
    );

    let report = tracker.poll_once().await.unwrap();
    // T1「第五节课 课前测」被忽略：不落 store、不建待办、不进 vault、不提醒
    assert_eq!(report.new_tasks, 3);
    assert!(
        store
            .document("zhihuishu_task", "职业生涯规划#T1")
            .unwrap()
            .is_none()
    );
    let sent = channel.sent.lock().unwrap();
    assert!(!sent[0].contains("课前测"), "忽略任务不应播报: {}", sent[0]);
    let files = vault.files.lock().unwrap();
    assert!(
        files.keys().all(|k| !k.contains("课前测")),
        "忽略任务不进 vault: {:?}",
        files.keys().collect::<Vec<_>>()
    );
    // 课程索引里也不出现
    let index = files
        .get("01-Courses-课程/2026-Fall-大一上/大学生职业生涯规划/智慧树作业.md")
        .unwrap();
    assert!(index.contains("未完成（2）"));
    assert!(!index.contains("课前测"));
}

#[tokio::test]
async fn optional_task_is_marked() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();
    let mut report_value = sample_report_json();
    // T1 临期（+1 天）且正文含选做信号 → 标注但待办不升 High
    report_value["courses"][0]["tasks"][0]["content"] =
        json!("选做：设置三个习题，用于回顾第四节的知识点。");
    let path = write_report(dir.path(), &report_value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    tracker.poll_once().await.unwrap();
    let sent = channel.sent.lock().unwrap();
    assert!(
        sent[0].contains("第五节课 课前测（选做）"),
        "选做任务应在提醒中标注: {}",
        sent[0]
    );
    let snap = store
        .document("zhihuishu_task", "职业生涯规划#T1")
        .unwrap()
        .unwrap();
    let id = snap.value["todo_id"].as_str().unwrap();
    let doc = store.document("todo_item", id).unwrap().unwrap();
    let item: TodoItem = serde_json::from_value(doc.value).unwrap();
    assert!(item.title.contains("（选做）"));
    assert_eq!(item.priority, TodoPriority::Normal, "选做不因临期升 High");
}

#[tokio::test]
async fn vault_index_merges_courses_into_one_dir() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let vault = Arc::new(FakeVault::default());
    // 同一门课的两个开课实例（平台课名不同、映射到同一 vault 目录），
    // 索引必须合并渲染而不是互相覆写。
    let mut report = sample_report_json();
    report["courses"][0]["platform"] = json!("?"); // 顺带覆盖：未知平台不进 frontmatter
    let mut second = sample_report_json()["courses"][0].clone();
    second["title"] = json!("职业生涯规划（2026-2027学年第一学期）");
    second["content_published"] = json!(false);
    second["tasks"] = json!([{
        "activity_task_id": "X1",
        "title": "另一班次的专属作业",
        "kind": "assignment",
        "end_at": platform_time(2),
        "completed": false
    }]);
    report["courses"].as_array_mut().unwrap().push(second);
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker_with(&store, path, None, true, Some(vault.clone()));

    tracker.poll_once().await.unwrap();
    let files = vault.files.lock().unwrap();
    let index = files
        .get("01-Courses-课程/2026-Fall-大一上/大学生职业生涯规划/智慧树作业.md")
        .unwrap();
    // 合并视图：标题用目录名，两个班次的任务都在，未知平台不写 platform 行
    assert!(index.contains("# 大学生职业生涯规划 · 智慧树作业"));
    assert!(index.contains("course: 大学生职业生涯规划"));
    assert!(!index.contains("platform:"));
    assert!(
        index.contains("未完成（4）"),
        "两个班次的任务应合并: {index}"
    );
    assert!(index.contains("另一班次的专属作业"));
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
    // 过期提醒是一次性的：nag 记录不带日期，后续轮询不再重复播报
    assert!(
        store
            .document("zhihuishu_alert", "职业生涯规划#OLD1:overdue")
            .unwrap()
            .is_some()
    );
    let report2 = tracker.poll_once().await.unwrap();
    assert!(!report2.notified);
    assert_eq!(channel.sent.lock().unwrap().len(), 1);
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

use ru41_plugin_zhihuishu::{
    ZhihuishuConfig, ZhihuishuSection, ZhihuishuTracker, load_report, normalize_platform_time,
    parse_platform_time,
};

/// 毫秒 epoch 与 ISO 串必须收敛到同一个值：
/// 同一时刻的两种写法若被当成两个值, 就会误报「截止变更」。
#[test]
fn platform_time_normalization_unifies_epoch_and_iso() {
    // 1794628800000 ms == 1794628800 s（2026-11-14 12:00:00 +08:00）
    let expected = chrono::DateTime::from_timestamp(1794628800, 0)
        .unwrap()
        .with_timezone(&chrono::Local);
    let canonical = expected.format("%Y-%m-%d %H:%M:%S").to_string();

    assert_eq!(normalize_platform_time("1794628800000").unwrap(), canonical);
    assert_eq!(normalize_platform_time("1794628800").unwrap(), canonical);
    // 归一化后必须能被 parse_platform_time 解析回同一个 Unix 秒
    assert_eq!(
        parse_platform_time("1794628800000"),
        Some(expected.timestamp())
    );
    assert_eq!(
        parse_platform_time("1794628800"),
        Some(expected.timestamp())
    );
}

#[test]
fn platform_time_normalization_keeps_iso_and_rejects_garbage() {
    assert_eq!(
        normalize_platform_time("2026-11-14 12:00:00").unwrap(),
        "2026-11-14 12:00:00"
    );
    assert_eq!(
        normalize_platform_time("2026-11-14 12:00").unwrap(),
        "2026-11-14 12:00:00"
    );
    assert_eq!(
        normalize_platform_time("2026-11-14").unwrap(),
        "2026-11-14 00:00:00"
    );
    // 认不出来的一律 None，调用方保留原值而不是丢弃
    assert!(normalize_platform_time("未知").is_none());
    assert!(normalize_platform_time("").is_none());
    // 紧凑日期串不是 epoch，不能被误解析成 1970 年的时间
    assert!(normalize_platform_time("20261114").is_none());
}

/// 报告里的毫秒 epoch 串在读取时就被归一化（sidecar 漏归一化时的兜底）。
#[test]
fn load_report_normalizes_millisecond_end_at() {
    let dir = tempfile::tempdir().unwrap();
    let mut value = sample_report_json();
    value["courses"][0]["tasks"][2]["end_at"] = json!("1794628800000");
    let path = write_report(dir.path(), &value);

    let report = load_report(&path).unwrap();
    let canonical = chrono::DateTime::from_timestamp(1794628800, 0)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    assert_eq!(
        report.courses[0].tasks[2].end_at.as_deref(),
        Some(canonical.as_str())
    );
}

/// 回归：快照里残留的毫秒串（sidecar 归一化前的老库）与本轮报告是同一时刻，
/// 不该报「截止变更」；同时快照要被自愈成归一化格式。
#[tokio::test]
async fn legacy_millisecond_snapshot_does_not_fake_deadline_change() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();

    // 报告里的 end_at 是毫秒 epoch 串（sidecar 归一化前的形态）
    let mut value = sample_report_json();
    value["courses"][0]["tasks"][2]["end_at"] = json!("1794628800000");
    let path = write_report(dir.path(), &value);
    let tracker = make_tracker(&store, path, Some(channel.clone()));

    tracker.poll_once().await.unwrap();

    // 人为把快照改回历史遗留的毫秒串，模拟归一化前的老库
    let key = "职业生涯规划#T3";
    let snap = store.document("zhihuishu_task", key).unwrap().unwrap();
    let mut dirty = snap.value.clone();
    dirty["end_at"] = json!("1794628800000");
    store
        .put_document("zhihuishu_task", key, Some(snap.revision), &dirty)
        .unwrap();

    let before = channel.sent.lock().unwrap().len();
    tracker.poll_once().await.unwrap();

    let sent = channel.sent.lock().unwrap().clone();
    let fresh: Vec<&String> = sent[before..].iter().collect();
    assert!(
        !fresh.iter().any(|m| m.contains("截止变更")),
        "毫秒串与 ISO 串是同一时刻，不该报截止变更：{fresh:?}"
    );

    // 快照被自愈成归一化格式，不再是裸毫秒
    let snap = store.document("zhihuishu_task", key).unwrap().unwrap();
    let end_at = snap.value["end_at"].as_str().unwrap();
    let canonical = chrono::DateTime::from_timestamp(1794628800, 0)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    assert_eq!(end_at, canonical, "快照仍存着裸毫秒: {end_at}");
}

// ── 2026-11-13 生产实况回归 ─────────────────────────────────────────────
// 用户反馈「智慧树上已完成的作业，/作业 里还在」。根因有三类，
// 这几个用例分别钉住：完成态被猜成 false、平台换 id 后的僵尸快照、
// 以及重开时旧待办不关导致的重复条目。

/// 报告里完成态为 null（平台没给）时，不得把「已完成」翻转成「未完成」，
/// 也不得重建待办——未知不是「老师打回」。
#[tokio::test]
async fn unknown_completion_never_reopens_a_done_task() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());
    let channel = mock_channel();

    // 预置：快照已完成 + 待办已回勾。
    store
        .put_document(
            "zhihuishu_task",
            "职业生涯规划#T1",
            None,
            &json!({
                "course": "职业生涯规划",
                "title": "第五节课 课前测",
                "kind": "assignment",
                "end_at": platform_time(1),
                "completed": true,
                "exam_style": false,
                "todo_id": "todo-seed",
                "todo_synced": true,
                "first_seen": 1,
            }),
        )
        .unwrap();

    // 本轮报告：完成态未知（null）。
    let mut report = sample_report_json();
    report["courses"][0]["tasks"][0]["completed"] = json!(null);
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker(&store, path, Some(channel.clone()));
    tracker.poll_once().await.unwrap();

    let snap = store
        .document("zhihuishu_task", "职业生涯规划#T1")
        .unwrap()
        .unwrap();
    assert_eq!(
        snap.value["completed"],
        json!(true),
        "完成态未知不得把已完成翻回未完成: {snap:?}"
    );
    // 不得为 T1 重建待办（报告里其它未完成作业的待办不算）。
    let t1_todos = store
        .documents("todo_item", 100)
        .unwrap()
        .iter()
        .filter(|d| {
            d.value["title"]
                .as_str()
                .unwrap()
                .contains("第五节课 课前测")
        })
        .count();
    assert_eq!(t1_todos, 0, "未知完成态不该重建待办");
    let sent = channel.sent.lock().unwrap();
    assert!(
        !sent.iter().any(|m| m.contains("作业重新开放")),
        "未知完成态不该推送重开提醒: {sent:?}"
    );
}

/// 平台把作业重发一遍会换 activityTaskId：旧 key 的快照必须归档，
/// 否则 /作业 会永远挂一条其实早就交完的作业（德法第一次线上作业）。
#[tokio::test]
async fn replaced_task_id_archives_stale_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());

    // 旧 id 的快照：未完成 + 带待办。
    store
        .put_document(
            "zhihuishu_task",
            "26-27-1思想道德与法治#OLD-ID",
            None,
            &json!({
                "course": "26-27-1思想道德与法治",
                "title": "德法第一次线上作业",
                "kind": "assignment",
                "end_at": platform_time(-2),
                "completed": false,
                "exam_style": false,
                "todo_id": "todo-old",
                "first_seen": 1,
            }),
        )
        .unwrap();

    // 本轮报告：同课程同标题，但换了新 id，且已完成。
    let report = json!({
        "generated_at": "2026-11-13 10:00:00",
        "login_ok": true,
        "courses": [{
            "title": "26-27-1思想道德与法治",
            "platform": "polymas",
            "course_home_url": null,
            "content_published": null,
            "tasks": [{
                "activity_task_id": "NEW-ID",
                "title": "德法第一次线上作业",
                "kind": "assignment",
                "end_at": platform_time(1),
                "completed": true
            }]
        }]
    });
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker(&store, path, None);
    let poll = tracker.poll_once().await.unwrap();

    let stale = store
        .document("zhihuishu_task", "26-27-1思想道德与法治#OLD-ID")
        .unwrap()
        .unwrap();
    assert_eq!(
        stale.value["archived"],
        json!(true),
        "旧 id 的快照没被归档: {stale:?}"
    );
    assert_eq!(
        stale.value["superseded_by"],
        json!("26-27-1思想道德与法治#NEW-ID")
    );
    assert_eq!(poll.archived, 1);

    // 新 id 的快照正常入库且为已完成。
    let fresh = store
        .document("zhihuishu_task", "26-27-1思想道德与法治#NEW-ID")
        .unwrap()
        .unwrap();
    assert_eq!(fresh.value["completed"], json!(true));
    assert!(fresh.value.get("archived").is_none());
}

/// 真的重开（老师打回）时：旧待办要么被作废、要么保留为已完成，
/// 但同一条作业只允许留一条 active 待办（生产实况里出现过两条 active 重复条目）。
#[tokio::test]
async fn reopen_leaves_exactly_one_active_todo() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());

    let mut report = sample_report_json();
    // 首轮：未完成 → 建待办。
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker(&store, path, None);
    tracker.poll_once().await.unwrap();

    let t1_todo = store
        .documents("todo_item", 100)
        .unwrap()
        .iter()
        .find(|d| {
            d.value["title"]
                .as_str()
                .unwrap()
                .contains("第五节课 课前测")
        })
        .expect("首轮应建待办")
        .id
        .clone();

    // 次轮：平台报已完成 → 回勾。
    report["courses"][0]["tasks"][0]["completed"] = json!(true);
    write_report(dir.path(), &report);
    tracker.poll_once().await.unwrap();
    let done_item: TodoItem = serde_json::from_value(
        store
            .document("todo_item", &t1_todo)
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(done_item.status, TodoStatus::Completed);

    // 三轮：老师打回 → 重开。旧的已完成待办保留为历史，redo 走新的 active 待办。
    report["courses"][0]["tasks"][0]["completed"] = json!(false);
    write_report(dir.path(), &report);
    tracker.poll_once().await.unwrap();

    let active_t1: Vec<String> = store
        .documents("todo_item", 100)
        .unwrap()
        .iter()
        .filter(|d| d.value["status"] == json!("active"))
        .filter(|d| {
            d.value["title"]
                .as_str()
                .unwrap()
                .contains("第五节课 课前测")
        })
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(
        active_t1.len(),
        1,
        "同一条作业应只剩一条 active 待办: {active_t1:?}"
    );
}

/// 重开时旧的待办还处于 active（平台把完成态吞了/回勾失败）→ 必须作废，
/// 否则待办列表出现两条同作业的 active 条目。
#[tokio::test]
async fn reopen_cancels_old_active_todo() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());

    // 预置：快照说已完成，但待办还挂在 active（回勾没成功过）。
    store
        .put_document(
            "todo_item",
            "todo-old",
            None,
            &serde_json::to_value(TodoItem {
                id: "todo-old".into(),
                title: "智慧树｜职业生涯规划｜第五节课 课前测".into(),
                status: TodoStatus::Active,
                priority: TodoPriority::Normal,
                labels: vec!["智慧树".into()],
                due_at: Some(ru41::util::now() + 86_400),
                remind_at: None,
                last_reminded_at: None,
                completed_at: None,
                created_at: 1,
                updated_at: 1,
                source: None,
            })
            .unwrap(),
        )
        .unwrap();
    store
        .put_document(
            "zhihuishu_task",
            "职业生涯规划#T1",
            None,
            &json!({
                "course": "职业生涯规划",
                "title": "第五节课 课前测",
                "kind": "assignment",
                "end_at": platform_time(1),
                "completed": true,
                "exam_style": false,
                "todo_id": "todo-old",
                "todo_synced": false,
                "first_seen": 1,
            }),
        )
        .unwrap();

    // 平台报未完成（老师打回）→ 重开：旧待办作废 + 新建一条 active。
    let mut report = sample_report_json();
    report["courses"][0]["tasks"][0]["completed"] = json!(false);
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker(&store, path, None);
    tracker.poll_once().await.unwrap();

    let old_item: TodoItem = serde_json::from_value(
        store
            .document("todo_item", "todo-old")
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(
        old_item.status,
        TodoStatus::Cancelled,
        "重开时旧待办没作废，待办列表会留重复条目"
    );

    let active_t1: Vec<String> = store
        .documents("todo_item", 100)
        .unwrap()
        .iter()
        .filter(|d| d.value["status"] == json!("active"))
        .filter(|d| {
            d.value["title"]
                .as_str()
                .unwrap()
                .contains("第五节课 课前测")
        })
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(active_t1.len(), 1, "重开后应只有一条 active 待办");
}

/// 历史遗留：同一条作业留了两条 active 待办（旧的重开没关旧的）→
/// 只保留快照当前指向的那条，另一条作废。
#[tokio::test]
async fn legacy_duplicate_todos_are_deduped() {
    let dir = tempfile::tempdir().unwrap();
    let store = make_store(dir.path());

    let mk_todo = |id: &str, title: &str| {
        serde_json::to_value(TodoItem {
            id: id.into(),
            title: title.into(),
            status: TodoStatus::Active,
            priority: TodoPriority::Normal,
            labels: vec!["智慧树".into()],
            due_at: Some(ru41::util::now() + 86_400),
            remind_at: None,
            last_reminded_at: None,
            completed_at: None,
            created_at: 1,
            updated_at: 1,
            source: Some(ru41::domain::todo::TodoSource {
                channel: "zhihuishu".into(),
                source_id: Some("T1".into()),
            }),
        })
        .unwrap()
    };
    store
        .put_document(
            "todo_item",
            "todo-old",
            None,
            &mk_todo("todo-old", "智慧树｜课前测"),
        )
        .unwrap();
    store
        .put_document(
            "todo_item",
            "todo-new",
            None,
            &mk_todo("todo-new", "智慧树｜课前测"),
        )
        .unwrap();
    store
        .put_document(
            "zhihuishu_task",
            "职业生涯规划#T1",
            None,
            &json!({
                "course": "职业生涯规划",
                "title": "第五节课 课前测",
                "kind": "assignment",
                "end_at": platform_time(1),
                "completed": false,
                "exam_style": false,
                "todo_id": "todo-new",
                "first_seen": 1,
            }),
        )
        .unwrap();

    let mut report = sample_report_json();
    report["courses"][0]["tasks"][0]["completed"] = json!(false);
    let path = write_report(dir.path(), &report);
    let tracker = make_tracker(&store, path, None);
    let poll = tracker.poll_once().await.unwrap();

    assert_eq!(poll.deduped, 1, "重复待办没被收口");
    let old: TodoItem = serde_json::from_value(
        store
            .document("todo_item", "todo-old")
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(old.status, TodoStatus::Cancelled);
    let new: TodoItem = serde_json::from_value(
        store
            .document("todo_item", "todo-new")
            .unwrap()
            .unwrap()
            .value,
    )
    .unwrap();
    assert_eq!(new.status, TodoStatus::Active, "当前待办不该被动");
}
