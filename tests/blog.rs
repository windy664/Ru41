use ru41::adapters::vault::GitVault;
use ru41::modules::blog::{BlogConfig, BlogPublishTracker};
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
        std::fs::create_dir_all(vault_seed.join("06-Blog-博客/Pending-待发布")).unwrap();
        std::fs::write(vault_seed.join("06-Blog-博客/Pending-待发布/.gitkeep"), "").unwrap();
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
                remote: "origin".into(),
                branch: "master".into(),
                pending_dir: "06-Blog-博客/Pending-待发布".into(),
                published_dir: "06-Blog-博客/Published-已发布".into(),
                posts_dir: "source/_posts".into(),
                site_url: "https://example.com".into(),
                interval_secs: 300,
                notify: Some(ru41::domain::NotifyRoute {
                    channel: "test".into(),
                    target: "test".into(),
                    msg_id: None,
                }),
            },
        )
    }

    /// 向 Vault 远端推入一个待发布文件（模拟 Obsidian 端同步）。
    fn push_pending(&self, name: &str, content: &str) {
        let seed = self._dir.path().join("vault-seed");
        let file = seed.join("06-Blog-博客/Pending-待发布").join(name);
        std::fs::write(&file, content).unwrap();
        git(&seed, &["add", "-A"]);
        git(&seed, &["commit", "-m", &format!("pending {name}")]);
        git(&seed, &["push", "origin", "master"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publishes_pending_post_end_to_end() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_pending("我的测试文章.md", POST);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "published");
    assert_eq!(report.published, 1);
    assert_eq!(report.rejected, 0);

    // 博客仓库收到规范命名的文章。
    let blog_seed = fx._dir.path().join("blog-seed");
    git(&blog_seed, &["fetch", "origin"]);
    let log = git(&blog_seed, &["log", "--format=%s", "origin/master", "-1"]);
    assert!(log.contains("[ru41] publish posts"), "{log}");
    let files = git(
        &blog_seed,
        &["show", "--name-only", "--format=", "origin/master"],
    );
    assert!(files.contains("source/_posts/2026-10-19-"), "{files}");

    // Vault：文件移入已发布目录，Pending 清空，[ru41] 提交。
    let vault_seed = fx._dir.path().join("vault-seed");
    git(&vault_seed, &["fetch", "origin"]);
    let pending = git(
        &vault_seed,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "origin/master",
            "06-Blog-博客/Pending-待发布",
        ],
    );
    assert!(!pending.contains("我的测试文章.md"), "{pending}");
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
    assert!(published.contains("2026-10-19-"), "{published}");
    let vlog = git(&vault_seed, &["log", "--format=%s", "origin/master", "-1"]);
    assert!(vlog.contains("[ru41] blog publish"), "{vlog}");

    // 再次轮询：游标已推进，幂等不重复发布。
    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "idle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_invalid_frontmatter_without_blocking_others() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_pending("坏文章.md", POST_BAD_CATEGORY);
    fx.push_pending("好文章.md", POST);

    let report = tracker.poll_once().await.unwrap();
    assert_eq!(report.action, "published");
    assert_eq!(report.published, 1);
    assert_eq!(report.rejected, 1);

    // 坏文件留在 Pending（不被移动），好文件已发布。
    let vault_seed = fx._dir.path().join("vault-seed");
    git(&vault_seed, &["fetch", "origin"]);
    let pending = git(
        &vault_seed,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "origin/master",
            "06-Blog-博客/Pending-待发布",
        ],
    );
    assert!(pending.contains("坏文章.md"), "{pending}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_push_retries_next_poll() {
    let fx = Fx::new();
    let tracker = fx.tracker();
    fx.push_pending("文章.md", POST);
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
