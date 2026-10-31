use ru41::adapters::database::sqlite::SqliteTaskStore;
use ru41::adapters::vault::{EMPTY_TREE, GitVault};
use ru41::app::obsidian::{ObsidianConfig, ObsidianService, append_quick_note};
use ru41::domain::AgentResult;
use ru41::ports::storage::TaskStore;
use ru41::ports::vault::Vault;
use std::path::PathBuf;
use std::sync::Arc;

mod common;
use common::git::git;

struct RepoFixture {
    _dir: tempfile::TempDir,
    _remote: PathBuf,
    seed: PathBuf,
    vault_path: PathBuf,
    store: Arc<dyn TaskStore>,
}

impl RepoFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let seed = dir.path().join("seed");
        let vault_path = dir.path().join("vault");
        git(
            dir.path(),
            &["init", "--bare", "-b", "master", remote.to_str().unwrap()],
        );
        git(
            dir.path(),
            &["clone", remote.to_str().unwrap(), seed.to_str().unwrap()],
        );
        git(&seed, &["config", "user.name", "windy"]);
        git(&seed, &["config", "user.email", "windy@test"]);
        std::fs::create_dir_all(seed.join("03-Knowledge-知识")).unwrap();
        std::fs::write(seed.join("03-Knowledge-知识/已有笔记.md"), "旧内容").unwrap();
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-m", "init"]);
        git(&seed, &["push", "origin", "master"]);
        git(
            dir.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                vault_path.to_str().unwrap(),
            ],
        );
        // tracker 的提交发生在这个副本里（走生产 GitVault，不经测试 helper），
        // 必须配 repo 级身份——CI runner 没有全局 git identity。
        git(&vault_path, &["config", "user.name", "windy"]);
        git(&vault_path, &["config", "user.email", "windy@test"]);
        let store: Arc<dyn TaskStore> =
            Arc::new(SqliteTaskStore::open(&dir.path().join("state.db")).unwrap());
        Self {
            _dir: dir,
            _remote: remote,
            seed,
            vault_path,
            store,
        }
    }

    fn vault(&self) -> GitVault {
        GitVault::new(&self.vault_path, "origin", "master")
    }

    fn tracker_with(
        &self,
        notifier: Option<Arc<dyn ru41::ports::channel::NotifyChannel>>,
    ) -> ObsidianService {
        let route = ru41::domain::NotifyRoute {
            channel: "test".into(),
            target: "test".into(),
            msg_id: None,
        };
        ObsidianService::new(
            self.store.clone(),
            Arc::new(self.vault()),
            notifier,
            ObsidianConfig {
                vault_path: self.vault_path.clone(),
                remote: "origin".into(),
                branch: "master".into(),
                inbox_dir: "00-Inbox-收件箱".into(),
                interval_secs: 300,
                backend: "codex".into(),
                notify: Some(route),
                merge: None,
            },
        )
    }

    fn tracker(&self) -> ObsidianService {
        let route = ru41::domain::NotifyRoute {
            channel: "test".into(),
            target: "test".into(),
            msg_id: None,
        };
        self.tracker_with_route(None, route)
    }

    fn tracker_with_route(
        &self,
        notifier: Option<Arc<dyn ru41::ports::channel::NotifyChannel>>,
        route: ru41::domain::NotifyRoute,
    ) -> ObsidianService {
        ObsidianService::new(
            self.store.clone(),
            Arc::new(self.vault()),
            notifier,
            ObsidianConfig {
                vault_path: self.vault_path.clone(),
                remote: "origin".into(),
                branch: "master".into(),
                inbox_dir: "00-Inbox-收件箱".into(),
                interval_secs: 300,
                backend: "codex".into(),
                notify: Some(route),
                merge: None,
            },
        )
    }

    /// 在 seed 仓库提交并推送一个变更。
    fn push_change(&self, path: &str, content: &str, message: &str) -> String {
        let file = self.seed.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content).unwrap();
        git(&self.seed, &["add", "-A"]);
        git(&self.seed, &["commit", "-m", message]);
        git(&self.seed, &["push", "origin", "master"]);
        git(&self.seed, &["rev-parse", "HEAD"])
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_vault_basic_ops() {
    let fx = RepoFixture::new();
    let vault = fx.vault();
    vault.fetch().unwrap();
    let head = vault.remote_head().unwrap();
    assert_eq!(vault.head_subject(&head).unwrap(), "init");
    // 尚无基线时以空树为起点，等价于全部文件为新增。
    assert_eq!(vault.changed_files(EMPTY_TREE, &head).unwrap().len(), 1);
    // 无变更时 commit_all 返回 false。
    assert!(!vault.commit_all("noop").unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbox_change_enqueues_organize_task() {
    let fx = RepoFixture::new();
    let tracker = fx.tracker();
    // 基线为空时首跑：现有 Inbox 内容入队整理。
    fx.push_change("00-Inbox-收件箱/新想法.md", "一个想法", "想法");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "enqueued");
    assert_eq!(report.changed_files, 1);
    let tasks = fx.store.list(10).unwrap();
    assert_eq!(tasks.len(), 1);
    assert!(tasks[0].prompt.contains("00-Inbox-收件箱/新想法.md"));
    // 再次轮询：任务进行中，不重复入队。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "waiting");
    assert_eq!(fx.store.list(10).unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn self_commit_advances_cursor_without_task() {
    let fx = RepoFixture::new();
    let tracker = fx.tracker();
    fx.push_change(
        "00-Inbox-收件箱/整理结果.md",
        "已整理",
        "[ru41] organize inbox abc123",
    );
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "self-commit");
    assert!(fx.store.list(10).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_inbox_change_only_advances_cursor() {
    let fx = RepoFixture::new();
    let tracker = fx.tracker();
    fx.push_change("03-Knowledge-知识/新笔记.md", "知识", "笔记");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "skipped");
    assert!(fx.store.list(10).unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn succeeded_task_commits_and_pushes() {
    let fx = RepoFixture::new();
    let tracker = fx.tracker();
    fx.push_change("00-Inbox-收件箱/新想法.md", "一个想法", "想法");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "enqueued");

    // 模拟 Agent 完成整理：移动文件 + 任务标记成功。
    std::fs::create_dir_all(fx.vault_path.join("03-Knowledge-知识")).unwrap();
    std::fs::rename(
        fx.vault_path.join("00-Inbox-收件箱/新想法.md"),
        fx.vault_path.join("03-Knowledge-知识/新想法.md"),
    )
    .unwrap();
    let claim = fx.store.claim("worker", 100, 30).unwrap().unwrap();
    fx.store
        .succeed(
            &claim,
            &AgentResult {
                summary: "done".into(),
            },
            None,
            101,
        )
        .unwrap();

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "committed");
    assert!(report.committed);
    // 远端已收到 Ru41 的整理提交（后一条是自动更新的整理索引）。
    git(&fx.seed, &["fetch", "origin"]);
    let log = git(&fx.seed, &["log", "--format=%s", "origin/master", "-2"]);
    assert!(log.contains("[ru41] organize inbox"), "{log}");
    // 整理索引已生成并推送：记录本次归档去向。
    let index = git(
        &fx.seed,
        &["show", "origin/master:00-Inbox-收件箱/整理索引.md"],
    );
    assert!(index.contains("整理索引"), "{index}");
    assert!(index.contains("新想法.md"), "{index}");
    assert!(index.contains("03-Knowledge-知识/新想法.md"), "{index}");
    // 游标推进后再次轮询：ru41 自己的提交只走 self-commit。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "self-commit");
    assert!(fx.store.list(10).unwrap().len() == 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recent_inbox_moves_reports_archives_and_deletes() {
    let fx = RepoFixture::new();
    let vault = fx.vault();
    // 两份收件箱文件：一份将被归档（改名移动），一份将被删除（转待办场景）。
    fx.push_change("00-Inbox-收件箱/讲座笔记.md", "笔记", "add a");
    fx.push_change("00-Inbox-收件箱/一句话待办.md", "待办", "add b");
    vault.fetch().unwrap();
    vault.checkout_hard(&vault.remote_head().unwrap()).unwrap();

    // 模拟 agent 归档：移动 + 删除，按生产路径 commit_all（[ru41] 提交）。
    std::fs::create_dir_all(
        fx.vault_path
            .join("05-Opportunities-机会/Competitions-竞赛"),
    )
    .unwrap();
    std::fs::rename(
        fx.vault_path.join("00-Inbox-收件箱/讲座笔记.md"),
        fx.vault_path
            .join("05-Opportunities-机会/Competitions-竞赛/讲座笔记.md"),
    )
    .unwrap();
    std::fs::remove_file(fx.vault_path.join("00-Inbox-收件箱/一句话待办.md")).unwrap();
    assert!(vault.commit_all("[ru41] organize inbox test").unwrap());

    let moves = vault.recent_inbox_moves("00-Inbox-收件箱", 7).unwrap();
    assert_eq!(moves.len(), 2, "{moves:?}");
    let archived = moves
        .iter()
        .find(|m| m.name == "讲座笔记.md")
        .expect("archived file recorded");
    assert_eq!(
        archived.destination,
        "05-Opportunities-机会/Competitions-竞赛/讲座笔记.md"
    );
    let deleted = moves
        .iter()
        .find(|m| m.name == "一句话待办.md")
        .expect("deleted file recorded");
    assert!(deleted.destination.contains("已删除"), "{deleted:?}");
    // 程序自身的索引文件永远不会出现在记录里。
    assert!(moves.iter().all(|m| m.name != "整理索引.md"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflict_aborts_when_remote_moved() {
    let fx = RepoFixture::new();
    let tracker = fx.tracker();
    fx.push_change("00-Inbox-收件箱/新想法.md", "一个想法", "想法");
    tracker.poll_once().await.unwrap();
    let claim = fx.store.claim("worker", 100, 30).unwrap().unwrap();
    fx.store
        .succeed(
            &claim,
            &AgentResult {
                summary: "done".into(),
            },
            None,
            101,
        )
        .unwrap();
    // 任务成功后、提交前远端又有新提交。
    fx.push_change("00-Inbox-收件箱/另一条.md", "另一条想法", "想法2");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "conflict");
    // pending 已清空，下一轮对新 HEAD 重新入队。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "enqueued");
    assert_eq!(fx.store.list(10).unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quick_note_appends_and_pushes() {
    let fx = RepoFixture::new();
    let vault = fx.vault();
    let path = append_quick_note(
        &vault,
        "00-Inbox-收件箱",
        "想到了一个点子",
        1_792_000_000,
        8 * 3600,
    )
    .unwrap();
    assert_eq!(path, "00-Inbox-收件箱/快速记录.md");
    git(&fx.seed, &["fetch", "origin"]);
    let content = git(
        &fx.seed,
        &["show", "origin/master:00-Inbox-收件箱/快速记录.md"],
    );
    assert!(content.contains("想到了一个点子"));
}

#[derive(Default)]
struct Recorder {
    sends: std::sync::Mutex<Vec<String>>,
}

impl ru41::ports::channel::NotifyChannel for Recorder {
    fn channel_id(&self) -> &str {
        "recorder"
    }

    fn send<'a>(
        &'a self,
        _target: &'a str,
        _msg_id: Option<&'a str>,
        text: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            self.sends.lock().unwrap().push(text.to_owned());
            Ok(())
        })
    }
}

fn fail_current_task(store: &Arc<dyn TaskStore>, error: &str) {
    let claim = store.claim("worker", 100, 30).unwrap().unwrap();
    store.fail(&claim, error, 101, 10, false, false).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failure_notifies_once_retries_then_abandons() {
    let fx = RepoFixture::new();
    let recorder = Arc::new(Recorder::default());
    let tracker = fx.tracker_with(Some(recorder.clone()));
    let sends = || recorder.sends.lock().unwrap().len();

    fx.push_change("00-Inbox-收件箱/想法.md", "想法", "想法");
    assert_eq!(tracker.poll_once().await.unwrap().action, "enqueued");

    // 第一次失败：通知一次并自动重试（任务回到 queued）。
    fail_current_task(&fx.store, "model error");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "retry");
    assert!(report.notified);
    assert_eq!(sends(), 1);
    assert_eq!(
        fx.store.list(10).unwrap()[0].status,
        ru41::domain::TaskStatus::Queued
    );

    // 重试后再失败：同一提交不重复通知。
    fail_current_task(&fx.store, "model error again");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "retry");
    assert!(!report.notified);
    assert_eq!(sends(), 1);

    // 第三次失败：超限放弃，发放弃通知，之后不再自动入队。
    fail_current_task(&fx.store, "still failing");
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "abandoned");
    assert_eq!(sends(), 2);
    assert!(recorder.sends.lock().unwrap()[0].contains("整理失败"));

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "abandoned");
    assert_eq!(sends(), 2);
}

#[test]
fn vault_sync_conflict_resolve_continue_flow() {
    use ru41::ports::vault::SyncOutcome;
    let fx = RepoFixture::new();

    // 基线文件推到远端，vault 同步对齐。
    std::fs::write(fx.seed.join("note.md"), "行1\n行2\n行3\n").unwrap();
    git(&fx.seed, &["add", "-A"]);
    git(&fx.seed, &["commit", "-m", "base"]);
    git(&fx.seed, &["push", "origin", "master"]);
    assert_eq!(fx.vault().sync().unwrap(), SyncOutcome::Clean);

    // 双侧改同一行制造冲突：vault 本地提交，seed 推远端。
    std::fs::write(fx.vault_path.join("note.md"), "行1\n本地改\n行3\n").unwrap();
    fx.vault().commit_all("[ru41] 本地改").unwrap();
    std::fs::write(fx.seed.join("note.md"), "行1\n远端改\n行3\n").unwrap();
    git(&fx.seed, &["add", "-A"]);
    git(&fx.seed, &["commit", "-m", "[local] 远端改"]);
    git(&fx.seed, &["push", "origin", "master"]);

    // sync 应保留冲突现场而非 abort。
    match fx.vault().sync().unwrap() {
        SyncOutcome::Conflict(files) => assert_eq!(files, vec!["note.md".to_string()]),
        other => panic!("expected Conflict, got {other:?}"),
    }
    // 冲突文件带标记。
    let conflicted = std::fs::read_to_string(fx.vault_path.join("note.md")).unwrap();
    assert!(conflicted.contains("<<<<<<<"));

    // 模拟 AI 合并结果写入 → continue → rebase 完成 → push。
    fx.vault()
        .resolve_conflict("note.md", "行1\n本地改+远端改\n行3\n")
        .unwrap();
    assert!(fx.vault().rebase_continue().unwrap().is_empty());
    fx.vault().push().unwrap();

    // 远端拿到合并结果；工作区干净。
    git(&fx.seed, &["pull", "-q", "origin", "master"]);
    assert_eq!(
        std::fs::read_to_string(fx.seed.join("note.md")).unwrap(),
        "行1\n本地改+远端改\n行3\n"
    );
    assert_eq!(fx.vault().sync().unwrap(), SyncOutcome::Clean);
}

#[test]
fn vault_sync_conflict_abort_restores() {
    use ru41::ports::vault::SyncOutcome;
    let fx = RepoFixture::new();
    std::fs::write(fx.seed.join("note.md"), "原文\n").unwrap();
    git(&fx.seed, &["add", "-A"]);
    git(&fx.seed, &["commit", "-m", "base"]);
    git(&fx.seed, &["push", "origin", "master"]);
    fx.vault().sync().unwrap();

    std::fs::write(fx.vault_path.join("note.md"), "本地\n").unwrap();
    fx.vault().commit_all("[ru41] 本地").unwrap();
    std::fs::write(fx.seed.join("note.md"), "远端\n").unwrap();
    git(&fx.seed, &["add", "-A"]);
    git(&fx.seed, &["commit", "-m", "[local] 远端"]);
    git(&fx.seed, &["push", "origin", "master"]);

    assert!(matches!(
        fx.vault().sync().unwrap(),
        SyncOutcome::Conflict(_)
    ));
    fx.vault().rebase_abort().unwrap();
    // abort 后本地提交还在（只是没 rebase 上去），工作区无冲突标记。
    assert_eq!(
        std::fs::read_to_string(fx.vault_path.join("note.md")).unwrap(),
        "本地\n"
    );
}
