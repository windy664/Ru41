use ru41::adapters::vault::GitVault;
use ru41_plugin_blog::{BlogConfig, BlogPublishTracker};
use std::path::PathBuf;
use std::sync::Arc;

mod common;
use common::git::git;

const POST: &str = "---\ntitle: 测试文章\ndate: 2026-10-19 20:00:00\ntags: [测试]\ncategories: [技术分享]\n---\n\n正文。\n";
const POST_BAD_CATEGORY: &str =
    "---\ntitle: 坏分类\ndate: 2026-10-19 20:00:00\ncategories: [不存在]\n---\n\n正文。\n";

struct Fx {
    _dir: tempfile::TempDir,
    _vault_remote: PathBuf,
    _blog_remote: PathBuf,
    vault_path: PathBuf,
    blog_path: PathBuf,
    store: Arc<dyn ru41::ports::storage::TaskStore>,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let vault_remote = dir.path().join("vault.git");
        let blog_remote = dir.path().join("blog.git");
        let vault_seed = dir.path().join("vault-seed");
        let blog_seed = dir.path().join("blog-seed");
        let vault_path = dir.path().join("vault");
        let blog_path = dir.path().join("blog");
        for remote in [&vault_remote, &blog_remote] {
            git(
                dir.path(),
                &["init", "--bare", "-b", "master", remote.to_str().unwrap()],
            );
        }
        git(
            dir.path(),
            &[
                "clone",
                vault_remote.to_str().unwrap(),
                vault_seed.to_str().unwrap(),
            ],
        );
        git(&vault_seed, &["config", "user.name", "windy"]);
        git(&vault_seed, &["config", "user.email", "w@test"]);
        std::fs::create_dir_all(vault_seed.join("06-Blog-博客/Published-已发布")).unwrap();
        std::fs::write(
            vault_seed.join("06-Blog-博客/Published-已发布/.gitkeep"),
            "",
        )
        .unwrap();
        git(&vault_seed, &["add", "-A"]);
        git(&vault_seed, &["commit", "-m", "init"]);
        git(&vault_seed, &["push", "origin", "master"]);

        git(
            dir.path(),
            &[
                "clone",
                blog_remote.to_str().unwrap(),
                blog_seed.to_str().unwrap(),
            ],
        );
        git(&blog_seed, &["config", "user.name", "windy"]);
        git(&blog_seed, &["config", "user.email", "w@test"]);
        git(&blog_seed, &["commit", "--allow-empty", "-m", "init"]);
        git(&blog_seed, &["push", "origin", "master"]);

        git(
            dir.path(),
            &[
                "clone",
                vault_remote.to_str().unwrap(),
                vault_path.to_str().unwrap(),
            ],
        );
        git(
            dir.path(),
            &[
                "clone",
                blog_remote.to_str().unwrap(),
                blog_path.to_str().unwrap(),
            ],
        );
        // clone 出来的工作副本也要配身份：tracker 的提交发生在副本里，
        // CI  runner 没有全局 git identity，不配会提交失败（本地靠用户全局配置兜住）。
        for repo in [&vault_path, &blog_path] {
            git(repo, &["config", "user.name", "windy"]);
            git(repo, &["config", "user.email", "w@test"]);
        }
        let store: Arc<dyn ru41::ports::storage::TaskStore> = Arc::new(
            ru41::adapters::database::sqlite::SqliteTaskStore::open(&dir.path().join("state.db"))
                .unwrap(),
        );
        Self {
            _dir: dir,
            _vault_remote: vault_remote,
            _blog_remote: blog_remote,
            vault_path,
            blog_path,
            store,
        }
    }

    fn tracker(&self) -> BlogPublishTracker {
        BlogPublishTracker::new(
            self.store.clone(),
            Arc::new(GitVault::new(&self.vault_path, "origin", "master")),
            Arc::new(GitVault::new(&self.blog_path, "origin", "master")),
            None,
            BlogConfig {
                repo_path: self.blog_path.clone(),
                site_url: "https://example.com".into(),
                notify: Some(ru41::domain::NotifyRoute {
                    channel: "test".into(),
                    target: "test".into(),
                    msg_id: None,
                }),
                ..Default::default()
            },
        )
    }

    /// 向 Vault 远端推入一个已发布目录新文件（模拟 Obsidian 端「移入并同步」）。
    fn push_published(&self, name: &str, content: &str) {
        let seed = self._dir.path().join("vault-seed");
        let file = seed.join("06-Blog-博客/Published-已发布").join(name);
        std::fs::write(&file, content).unwrap();
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-m", &format!("publish {name}")]);
        git(&seed, &["push", "origin", "master"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_published_dir_post_end_to_end() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_published("我的测试文章.md", POST);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "published");
    assert_eq!(report.published, 1);
    assert_eq!(report.rejected, 0);

    // 博客仓库收到规范命名的文章。
    let blog_seed = fx._dir.path().join("blog-seed");
    git(&blog_seed, &["fetch", "origin"]);
    let log = git(&blog_seed, &["log", "--format=%s", "origin/master", "-1"]);
    assert!(log.contains("[ru41] sync posts"), "{log}");
    let files = git(
        &blog_seed,
        &["show", "--name-only", "--format=", "origin/master"],
    );
    assert!(files.contains("source/_posts/2026-10-19-"), "{files}");

    // Vault：文件原地留在已发布目录，程序不回写 Vault（无 [ru41] 提交）。
    let vault_seed = fx._dir.path().join("vault-seed");
    git(&vault_seed, &["fetch", "origin"]);
    let published = git(
        &vault_seed,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "origin/master",
            "06-Blog-博客/Published-已发布",
        ],
    );
    assert!(published.contains("我的测试文章.md"), "{published}");
    let vlog = git(&vault_seed, &["log", "--format=%s", "origin/master", "-1"]);
    assert!(
        vlog.contains("publish 我的测试文章.md"),
        "Vault 应是用户自己的提交：{vlog}"
    );

    // 再次轮询：游标已推进，幂等不重复发布。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "idle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_frontmatter_without_blocking_others() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_published("坏文章.md", POST_BAD_CATEGORY);
    fx.push_published("好文章.md", POST);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "published");
    assert_eq!(report.published, 1);
    assert_eq!(report.rejected, 1);

    // 坏文件原地留在已发布目录（等人工修正），好文件进了博客仓库。
    let vault_seed = fx._dir.path().join("vault-seed");
    git(&vault_seed, &["fetch", "origin"]);
    let published = git(
        &vault_seed,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "origin/master",
            "06-Blog-博客/Published-已发布",
        ],
    );
    assert!(published.contains("坏文章.md"), "{published}");
    let blog_seed = fx._dir.path().join("blog-seed");
    git(&blog_seed, &["fetch", "origin"]);
    let files = git(
        &blog_seed,
        &["show", "--name-only", "--format=", "origin/master"],
    );
    assert!(files.contains("source/_posts/2026-10-19-"), "{files}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn moving_post_out_of_published_unpublishes_and_allows_republish() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_published("要下架的文章.md", POST);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.published, 1);

    // 移回草稿 = 下架：--no-renames 下旧路径以删除形式出现。
    let seed = fx._dir.path().join("vault-seed");
    std::fs::create_dir_all(seed.join("06-Blog-博客/Drafts-草稿")).unwrap();
    git(
        &seed,
        &[
            "mv",
            "06-Blog-博客/Published-已发布/要下架的文章.md",
            "06-Blog-博客/Drafts-草稿/要下架的文章.md",
        ],
    );
    git(&seed, &["commit", "-m", "move back to drafts"]);
    git(&seed, &["push", "origin", "master"]);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.unpublished, 1, "移出已发布目录应触发下架");
    assert_eq!(report.published, 0);

    // 博客仓库里文章已删除。
    let blog_seed = fx._dir.path().join("blog-seed");
    git(&blog_seed, &["fetch", "origin"]);
    let files = git(
        &blog_seed,
        &["ls-tree", "-r", "--name-only", "origin/master"],
    );
    assert!(!files.contains("要下架"), "{files}");

    // 再移回已发布：done 键已随下架清除，应能重新发布。
    git(
        &seed,
        &[
            "mv",
            "06-Blog-博客/Drafts-草稿/要下架的文章.md",
            "06-Blog-博客/Published-已发布/要下架的文章.md",
        ],
    );
    git(&seed, &["commit", "-m", "republish"]);
    git(&seed, &["push", "origin", "master"]);
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.published, 1, "下架后移回应能重新发布");
    git(&blog_seed, &["fetch", "origin"]);
    let files = git(
        &blog_seed,
        &["ls-tree", "-r", "--name-only", "origin/master"],
    );
    assert!(files.contains("要下架"), "{files}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_push_retries_next_poll() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_published("文章.md", POST);
    // 让博客仓库的 push 失败：把本地 blog clone 的 origin 指向不存在地址。
    git(
        &fx.blog_path,
        &[
            "remote",
            "set-url",
            "origin",
            fx._dir.path().join("nowhere.git").to_str().unwrap(),
        ],
    );

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "failed");
    // 游标未推进，下一轮重试（写文件幂等）。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "failed");
}

/// 游标漂移自愈：processed_sha 已指向 HEAD，但已发布目录里有从未发布的文章
/// （历史改写/目录整理/手工提交绕过 diff），对账应补发。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_publishes_when_cursor_skipped_ahead() {
    let fx = Fx::new();
    fx.push_published("对账补发文章.md", POST);
    git(&fx.vault_path, &["fetch", "origin"]);
    let head = git(&fx.vault_path, &["rev-parse", "origin/master"]);
    fx.store
        .put_document(
            "blog_publish_state",
            "main",
            None,
            &serde_json::json!({
                "processed_sha": head,
                "done": [],
                "published_map": {},
            }),
        )
        .unwrap();

    let report = fx.tracker().poll_once().await.unwrap();
    assert_eq!(report.action, "published");
    assert_eq!(report.published, 1);
    let files = git(&fx.blog_path, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(files.contains("2026-10-19-对账补发文章.md"), "{files}");
}

/// 对账按标题匹配而非文件名：历史手工提交用了不同文件名（英文 slug），
/// 同标题文章不应被当作缺失而重复发布。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconcile_matches_by_title_not_filename() {
    let fx = Fx::new();
    fx.push_published("2026-10-19-中文标题.md", POST);
    // 手工在博客仓库放一篇同标题不同文件名的文章（历史手工发布场景）。
    std::fs::create_dir_all(fx.blog_path.join("source/_posts")).unwrap();
    std::fs::write(fx.blog_path.join("source/_posts/english-slug.md"), POST).unwrap();
    git(&fx.blog_path, &["add", "-A"]);
    git(&fx.blog_path, &["commit", "-m", "manual post"]);
    // 游标已在 HEAD：diff 无内容，能否不重复发布全看对账。
    git(&fx.vault_path, &["fetch", "origin"]);
    let head = git(&fx.vault_path, &["rev-parse", "origin/master"]);
    fx.store
        .put_document(
            "blog_publish_state",
            "main",
            None,
            &serde_json::json!({
                "processed_sha": head,
                "done": [],
                "published_map": {},
            }),
        )
        .unwrap();

    let report = fx.tracker().poll_once().await.unwrap();
    assert_eq!(report.action, "idle");
    assert_eq!(report.published, 0);
}
