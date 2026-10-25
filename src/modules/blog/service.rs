use crate::{
    adapters::vault::EMPTY_TREE,
    ports::{channel::NotifyChannel, storage::TaskStore, vault::Vault},
};
use anyhow::{Context, Result};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// 博客发布运行时配置（由 BlogSection::resolved 产出）。
#[derive(Debug, Clone)]
pub struct BlogConfig {
    pub repo_path: PathBuf,
    pub remote: String,
    pub branch: String,
    /// Vault 内的已发布目录：用户把定稿移入该目录并推送即触发发布。
    pub published_dir: String,
    pub posts_dir: String,
    pub site_url: String,
    pub interval_secs: u64,
    pub notify: Option<crate::domain::NotifyRoute>,
}

impl Default for BlogConfig {
    /// 语义默认值与 BlogSection::resolved 对齐；测试构造只需写关心的字段。
    fn default() -> Self {
        Self {
            repo_path: std::path::PathBuf::new(),
            remote: "origin".into(),
            branch: "master".into(),
            published_dir: "06-Blog-博客/Published-已发布".into(),
            posts_dir: "source/_posts".into(),
            site_url: String::new(),
            interval_secs: 300,
            notify: None,
        }
    }
}


/// 一轮轮询的结果报告。
#[derive(Debug, Serialize)]
pub struct BlogPollReport {
    /// idle / published / partial / failed / waiting-config
    pub action: &'static str,
    pub remote_head: String,
    pub published: usize,
    pub rejected: usize,
    pub notified: bool,
}

/// 处理状态：records 文档 blog_publish_state:main。
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct BlogPublishState {
    #[serde(default)]
    processed_sha: String,
    /// 已处理（含校验拒绝）的已发布文件，键为 `路径:内容hash`，防止崩溃重试循环。
    #[serde(default)]
    done: Vec<String>,
    /// 最近一次失败已通知的 Vault HEAD（同一 HEAD 不重复通知）。
    #[serde(default)]
    notified_sha: Option<String>,
}

/// 已发布文件的校验结果。
struct PendingPost {
    content: String,
    /// 博客仓库文件名（YYYY-MM-DD-slug.md，URL 身份）。
    target_name: String,
    url: String,
}

/// 校验拒绝（会标记已处理并通知，不阻塞其他文件）。
struct RejectedPost {
    published_path: String,
    reason: String,
}

/// 博客发布追踪器：监听 Vault 已发布目录的新增 markdown，校验后写入博客仓库并推送。
/// 发布入口就是「把定稿从 Drafts-草稿 移入 Published-已发布 并推送」，没有中间队列。
pub struct BlogPublishTracker {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    blog: Arc<dyn Vault>,
    notifier: Option<Arc<dyn NotifyChannel>>,
    notify_target: String,
    config: BlogConfig,
}

impl BlogPublishTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        vault: Arc<dyn Vault>,
        blog: Arc<dyn Vault>,
        notifier: Option<Arc<dyn NotifyChannel>>,
        config: BlogConfig,
    ) -> Self {
        let notify_target = config
            .notify
            .as_ref()
            .map(|route| route.target.clone())
            .unwrap_or_default();
        Self {
            store,
            vault,
            blog,
            notifier,
            notify_target,
            config,
        }
    }

    fn load_state(&self) -> Result<BlogPublishState> {
        match self.store.document("blog_publish_state", "main")? {
            Some(doc) => {
                Ok(serde_json::from_value(doc.value).context("invalid blog_publish_state")?)
            }
            None => Ok(BlogPublishState::default()),
        }
    }

    fn save_state(&self, state: &BlogPublishState) -> Result<()> {
        let revision = self
            .store
            .document("blog_publish_state", "main")?
            .map(|d| d.revision);
        let written = self.store.put_document(
            "blog_publish_state",
            "main",
            revision,
            &serde_json::to_value(state)?,
        )?;
        anyhow::ensure!(written, "blog_publish_state 被并发修改，本轮状态未保存");
        Ok(())
    }

    pub async fn poll_once(&self) -> Result<BlogPollReport> {
        let vault = self.vault.clone();
        let head = tokio::task::spawn_blocking(move || -> Result<String> {
            vault.fetch()?;
            vault.remote_head()
        })
        .await??;
        let head_for_diff = head.clone();
        let mut state = self.load_state()?;
        let mut report = BlogPollReport {
            action: "idle",
            remote_head: head.clone(),
            published: 0,
            rejected: 0,
            notified: false,
        };
        if head == state.processed_sha {
            return Ok(report);
        }

        // 找出本轮新出现且未处理过的已发布文件。
        let vault = self.vault.clone();
        let from = state.processed_sha.clone();
        let published_prefix = format!("{}/", self.config.published_dir);
        let candidates = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
            let base = if from.is_empty() {
                EMPTY_TREE.to_owned()
            } else {
                from
            };
            Ok(vault
                .changed_files(&base, &head_for_diff)?
                .into_iter()
                .filter(|f| f.starts_with(&published_prefix) && f.ends_with(".md"))
                .collect())
        })
        .await??;
        let fresh: Vec<String> = candidates
            .into_iter()
            .filter(|path| !state.done.contains(&done_key(path)))
            .collect();
        if fresh.is_empty() {
            // 没有新的待发布文件：只推进游标（如普通笔记提交）。
            state.processed_sha = head.clone();
            self.save_state(&state)?;
            return Ok(report);
        }

        // 工作区对齐到远端 HEAD 后再读（fetch 只更新引用不动工作区）。
        let vault = self.vault.clone();
        let head_for_checkout = head.clone();
        tokio::task::spawn_blocking(move || vault.checkout_hard(&head_for_checkout)).await??;

        // 逐个读取并校验；校验失败的标记完成并收集原因。
        let vault = self.vault.clone();
        let paths = fresh.clone();
        let contents =
            tokio::task::spawn_blocking(move || -> Result<Vec<(String, Option<String>)>> {
                paths
                    .into_iter()
                    .map(|path| {
                        let content = vault.read_file(&path)?;
                        Ok((path, content))
                    })
                    .collect()
            })
            .await??;

        let mut posts: Vec<PendingPost> = Vec::new();
        let mut rejected: Vec<RejectedPost> = Vec::new();
        for (path, content) in contents {
            match content {
                Some(content) => match self.prepare_post(&path, &content) {
                    Ok(post) => posts.push(post),
                    Err(reason) => {
                        rejected.push(RejectedPost {
                            published_path: path,
                            reason,
                        });
                    }
                },
                None => rejected.push(RejectedPost {
                    published_path: path,
                    reason: "文件不存在或不是普通文件".into(),
                }),
            }
        }

        // 写入博客仓库并推送；任何一步失败保留游标下轮重试（写文件幂等）。
        if !posts.is_empty() {
            let blog = self.blog.clone();
            let writes: Vec<(String, String)> = posts
                .iter()
                .map(|p| {
                    (
                        format!("{}/{}", self.config.posts_dir, p.target_name),
                        p.content.clone(),
                    )
                })
                .collect();
            let config_path = self.config.repo_path.clone();
            let push_result = tokio::task::spawn_blocking(move || -> Result<()> {
                for (target, content) in &writes {
                    blog.write_file(target, content)?;
                }
                let _ = &config_path;
                // push 必须无条件执行：上一轮 push 失败时本地已存在未推送提交，
                // 本轮 commit_all 为空集不代表远端已同步。
                blog.commit_all("[ru41] publish posts")?;
                blog.push()?;
                Ok(())
            })
            .await?;
            if let Err(error) = push_result {
                let notify = state.notified_sha.as_deref() != Some(head.as_str());
                if notify {
                    state.notified_sha = Some(head.clone());
                    self.save_state(&state)?;
                    self.notify_failed(&posts, &format!("{error:#}")).await?;
                    report.notified = true;
                }
                report.action = "failed";
                return Ok(report);
            }
        }

        for path in &fresh {
            state.done.push(done_key(path));
        }
        state.processed_sha = head.clone();
        state.notified_sha = None;
        self.save_state(&state)?;

        report.published = posts.len();
        report.rejected = rejected.len();
        report.action = if posts.is_empty() {
            "partial"
        } else {
            "published"
        };
        if !posts.is_empty() || !rejected.is_empty() {
            self.notify_result(&posts, &rejected).await?;
            report.notified = true;
        }
        Ok(report)
    }

    /// 校验 frontmatter 并生成目标文件名与 URL。
    /// 规则对齐 src/content.config.ts：title 必填、categories 限枚举单值、tags ≤ 8。
    fn prepare_post(
        &self,
        published_path: &str,
        content: &str,
    ) -> std::result::Result<PendingPost, String> {
        let front = parse_frontmatter(content)
            .ok_or_else(|| "缺少 YAML frontmatter（须以 --- 开始）".to_owned())?;
        if !front.contains_key("title") {
            return Err("frontmatter 缺 title".to_owned());
        }
        let date = front
            .get("date")
            .and_then(|d| d.get(0..10).filter(|s| s.chars().take(10).count() == 10))
            .map(str::to_owned)
            .unwrap_or_else(today_label);
        if let Some(categories) = front.get("categories") {
            validate_categories(categories)?;
        }
        if let Some(tags) = front.get("tags") {
            let count = if tags.starts_with('[') {
                tags.trim_matches(|c| c == '[' || c == ']')
                    .split(',')
                    .filter(|t| !t.trim().is_empty() && *t != "[]")
                    .count()
            } else {
                1
            };
            if count > 8 {
                return Err(format!("tags 超过 8 个（{count}）"));
            }
        }
        if front.get("draft").map(|d| d == "true").unwrap_or(false) {
            return Err("frontmatter draft=true，请改为 false 再发布".to_owned());
        }
        let stem = published_path
            .rsplit('/')
            .next()
            .unwrap_or(published_path)
            .trim_end_matches(".md");
        let slug = sanitize_slug(stem);
        let target_name =
            if stem.len() >= 10 && stem.get(4..5) == Some("-") && stem.get(7..8) == Some("-") {
                format!("{slug}.md")
            } else {
                format!("{date}-{slug}.md")
            };
        let url = format!(
            "{}/{}/{}/{}/{}",
            self.config.site_url.trim_end_matches('/'),
            &date[0..4],
            &date[5..7],
            &date[8..10],
            target_name.trim_end_matches(".md")
        );
        Ok(PendingPost {
            content: content.to_owned(),
            target_name,
            url,
        })
    }

    async fn notify_result(&self, posts: &[PendingPost], rejected: &[RejectedPost]) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let mut lines = vec!["## 博客发布".to_owned()];
        for post in posts {
            lines.push(format!(
                "- ✅ {} → [打开文章]({})",
                post.target_name, post.url
            ));
        }
        for reject in rejected {
            lines.push(format!(
                "- ❌ `{}`：{}",
                reject.published_path, reject.reason
            ));
        }
        notifier
            .send(&self.notify_target, None, &lines.join("\n"))
            .await?;
        Ok(())
    }

    async fn notify_failed(&self, posts: &[PendingPost], error: &str) -> Result<()> {
        let Some(notifier) = &self.notifier else {
            return Ok(());
        };
        let names: Vec<&str> = posts.iter().map(|p| p.target_name.as_str()).collect();
        notifier
            .send(
                &self.notify_target,
                None,
                &format!(
                    "## 博客发布失败\n\n推送 {} 到博客仓库失败，下轮自动重试。\n\n> {}",
                    names.join("、"),
                    crate::util::truncate_chars(error, 300)
                ),
            )
            .await?;
        Ok(())
    }
}

impl crate::ingress::schedule::PollJob for BlogPublishTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}

/// 已处理键：路径 + 内容 hash（内容变化视为新文件重新处理）。
fn done_key(path: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    format!("{path}:{:016x}", hasher.finish())
}

fn today_label() -> String {
    crate::util::today_utc(crate::util::now())
}

/// 极简 frontmatter 行解析：只取 title/date/draft/categories/tags 的标量或单行数组。
fn parse_frontmatter(content: &str) -> Option<std::collections::HashMap<String, String>> {
    let text = content.strip_prefix("---\n")?;
    let end = text.find("\n---\n")?;
    let mut map = std::collections::HashMap::new();
    for line in text[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !matches!(key, "title" | "date" | "draft" | "categories" | "tags") {
            continue;
        }
        map.insert(
            key.to_owned(),
            value.trim().trim_matches('"').trim_matches('\'').to_owned(),
        );
    }
    Some(map)
}

/// categories 校验：单个值，且在 Astro 集合枚举内（string 或 [单值] 都接受）。
fn validate_categories(raw: &str) -> std::result::Result<(), String> {
    const ALLOWED: [&str; 7] = [
        "AI 应用",
        "工程化",
        "架构",
        "思考",
        "踩坑",
        "后端基础",
        "技术分享",
    ];
    let inner = raw
        .trim()
        .trim_matches(|c| c == '[' || c == ']')
        .trim_matches('"')
        .trim_matches('\'');
    let items: Vec<&str> = if raw.trim().starts_with('[') {
        inner
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        vec![inner.trim()]
    };
    if items.len() != 1 {
        return Err(format!("categories 必须是单值，当前 {raw}"));
    }
    let item = items[0].trim_matches('"').trim_matches('\'');
    if !ALLOWED.contains(&item) {
        return Err(format!(
            "categories 必须是 [{}] 之一，当前 `{item}`",
            ALLOWED.join(" / ")
        ));
    }
    Ok(())
}

/// slug 清洗：保留中英文字母数字与连字符，其余折叠为 '-'；空结果回退 post。
fn sanitize_slug(stem: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in stem.chars() {
        let ok = ch.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&ch) || ch == '-';
        if ok {
            out.extend(ch.to_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() {
        "post".to_owned()
    } else {
        out
    }
}

/// 供测试与文档引用的常量说明：Astro 集合允许的 categories 枚举。
pub const CATEGORIES: [&str; 7] = [
    "AI 应用",
    "工程化",
    "架构",
    "思考",
    "踩坑",
    "后端基础",
    "技术分享",
];
