use crate::{
    adapters::sources::feed::{FeedClient, FeedItem},
    domain::{NotifyRoute, TaskStatus},
    ports::{storage::TaskStore, vault::Vault},
};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

/// RSS 信息流追踪配置（对应配置文件的 [sources.rss]）。
#[derive(Debug, Clone)]
pub struct RssConfig {
    pub feeds: Vec<String>,
    /// 关键词粗筛（大小写不敏感，匹配标题或摘要）；空列表表示不过滤。
    pub keywords: Vec<String>,
    /// 个人关注方向描述，注入 digest prompt 供 Codex 判断相关性。
    pub interests: String,
    /// 每个源单次最多处理的条目数。
    pub max_items_per_feed: u32,
    pub interval_secs: u64,
    pub backend: String,
    pub notify: Option<NotifyRoute>,
}

/// 一轮轮询的结果报告；单源失败不中断其他源，错误收进 errors。
#[derive(Debug, Serialize)]
pub struct RssPollReport {
    pub feeds: usize,
    pub fetched: usize,
    pub new_items: usize,
    pub candidates: usize,
    pub errors: Vec<String>,
    pub digest_task_id: Option<i64>,
    /// 本轮归档进 05-Opportunities 的笔记路径。
    pub archived: Vec<String>,
}

/// RSS 信息流追踪：逐源抓取 → 去重 → 关键词粗筛 → 合成精选 digest 任务。
/// 与论文追踪同一模式，输出口径按设计文档 3.4：发生了什么→为什么重要→与我有关→下一步。
// TODO: 同一事件被多源报道时的聚合（当前由各源独立入 digest，靠 Agent 判断）。
pub struct RssTracker {
    store: Arc<dyn TaskStore>,
    client: FeedClient,
    config: RssConfig,
    /// 挂 Vault 后：每轮顺带把新完成的 digest 精选归档进 05-Opportunities。
    vault: Option<Arc<dyn Vault>>,
}

impl RssTracker {
    pub fn new(store: Arc<dyn TaskStore>, client: FeedClient, config: RssConfig) -> Self {
        Self {
            store,
            client,
            config,
            vault: None,
        }
    }

    /// 挂载机会雷达归档器；不挂则只抓 feed + 建 digest，不落 vault。
    pub fn with_vault(mut self, vault: Arc<dyn Vault>) -> Self {
        self.vault = Some(vault);
        self
    }

    pub async fn poll_once(&self) -> Result<RssPollReport> {
        let mut fetched = 0;
        let mut errors = Vec::new();
        let mut items = Vec::new();
        for url in &self.config.feeds {
            let feed_items = match self.client.fetch(url).await {
                Ok(feed_items) => feed_items,
                Err(error) => {
                    errors.push(format!("{url}: {error:#}"));
                    continue;
                }
            };
            fetched += feed_items.len();
            items.extend(
                feed_items
                    .into_iter()
                    .take(self.config.max_items_per_feed as usize)
                    .map(|item| (url.clone(), item)),
            );
        }
        let keywords = self.config.keywords.clone();
        let interests = self.config.interests.clone();
        let outcome = crate::app::digest::register_digest(
            crate::app::digest::DigestSink {
                store: &self.store,
                seen_kind: "rss_seen",
                backend: &self.config.backend,
                notify: self.config.notify.as_ref(),
            },
            items,
            crate::app::digest::DigestHooks {
                // 键带源前缀，避免不同源的 guid 互相碰撞。
                id: &|(url, item): &(String, FeedItem)| format!("{url}#{}", item.id),
                seen_value: &|(_, item): &(String, FeedItem)| {
                    serde_json::json!({
                        "title": item.title,
                        "link": item.link,
                        "first_seen": crate::util::now(),
                    })
                },
                keep: &|(_, item): &(String, FeedItem)| {
                    crate::app::digest::keyword_match(
                        &keywords,
                        &format!("{} {}", item.title, item.summary),
                    )
                },
                key: &|candidates: &[(String, FeedItem)]| {
                    format!(
                        "rss:digest:{}:{:08x}",
                        crate::util::today_utc(crate::util::now()),
                        digest_hash(candidates)
                    )
                },
                prompt: &|candidates: &[(String, FeedItem)]| {
                    digest_prompt(
                        &interests,
                        &candidates
                            .iter()
                            .map(|(_, item)| item.clone())
                            .collect::<Vec<_>>(),
                    )
                },
            },
        )?;
        let archived = self.archive_digests().unwrap_or_else(|error| {
            // 归档失败不影响 feed 轮询本体，下轮自然重试。
            eprintln!("[rss] opportunity archive failed: {error:#}");
            Vec::new()
        });
        Ok(RssPollReport {
            feeds: self.config.feeds.len(),
            fetched,
            new_items: outcome.new_items,
            candidates: outcome.candidates,
            errors,
            digest_task_id: outcome.digest_task_id,
            archived,
        })
    }

    /// 机会雷达归档：把已完成的 rss digest 精选写成
    /// `05-Opportunities-机会/雷达-日报/<日期>-digest-<task_id>.md`，
    /// 游标记录最后一个已归档任务 id（records 文档）。
    fn archive_digests(&self) -> Result<Vec<String>> {
        let Some(vault) = &self.vault else {
            return Ok(Vec::new());
        };
        let cursor = self
            .store
            .document("opportunity_archive", "cursor")?
            .and_then(|doc| doc.value["last_task_id"].as_i64())
            .unwrap_or(0);
        let tasks = self.store.list(200)?;
        let mut new_cursor = cursor;
        let mut written = Vec::new();
        for task in tasks.iter().filter(|task| {
            task.id > cursor
                && task.request_key.starts_with("rss:digest:")
                && task.status == TaskStatus::Succeeded
                && task.result.is_some()
        }) {
            let result = task.result.as_ref().expect("filtered by result.is_some");
            let date = task.updated_at.get(..10).unwrap_or("unknown-date");
            let path = format!(
                "05-Opportunities-机会/雷达-日报/{date}-digest-{}.md",
                task.id
            );
            let note = format!(
                "---\ndate: {date}\nsource: rss\ntask_id: {}\ntags:\n  - 机会\n  - 雷达\n---\n\n\
                 # 机会雷达精选（{date}）\n\n> 由 Ru41 机会雷达自动归档（{}），请勿手改。\n\n{}\n",
                task.id, task.request_key, result.summary
            );
            vault.write_file(&path, &note)?;
            written.push(path);
            new_cursor = new_cursor.max(task.id);
        }
        if !written.is_empty() {
            if vault.commit_all("[ru41] 机会雷达精选归档")? {
                vault.push()?;
            }
            self.store.put_document(
                "opportunity_archive",
                "cursor",
                None,
                &serde_json::json!({ "last_task_id": new_cursor }),
            )?;
        }
        Ok(written)
    }
}

/// 候选集合的稳定散列，用于 digest 幂等键（同一批重复 poll 不重复建任务）。
fn digest_hash(candidates: &[(String, FeedItem)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (url, item) in candidates {
        url.hash(&mut hasher);
        item.id.hash(&mut hasher);
    }
    hasher.finish()
}

/// 资讯精选 prompt：输出口径按设计文档 3.4 的四段式，区分事实与推断。
fn digest_prompt(interests: &str, candidates: &[FeedItem]) -> String {
    let mut prompt = format!(
        "你是信息筛选助手。以下是最新采集到的资讯条目（含标题与摘要级内容）。\n\
         个人关注方向：{interests}\n\n\
         请筛选出真正有信息增量、与关注方向相关的条目（搬运、纯营销、无实质内容的直接舍弃），\n\
         对每条输出：标题 / 链接 / 发生了什么 / 为什么重要 / 与我有关之处 / 建议的下一步行动。\n\
         区分事实与推断，事实必须能在所给材料中找到依据。\n\
         如果全部不值得关注，直接说明「本批次无值得关注的内容」。\n\n\
         == 候选条目（{} 条）==\n",
        candidates.len()
    );
    for item in candidates {
        prompt.push_str(&format!(
            "\n---\n{}\n链接：{}\n发布：{}\n内容：{}\n",
            item.title, item.link, item.published, item.summary
        ));
    }
    prompt
}

impl crate::ingress::schedule::PollJob for RssTracker {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async move { Ok(serde_json::to_value(self.poll_once().await?)?) })
    }
}
