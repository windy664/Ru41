use crate::{
    adapters::sources::arxiv::{ArxivClient, ArxivEntry},
    domain::NotifyRoute,
    ports::storage::TaskStore,
};
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

/// arXiv 追踪配置（对应配置文件的 [sources.arxiv]）。
#[derive(Debug, Clone)]
pub struct PapersConfig {
    pub categories: Vec<String>,
    /// 关键词粗筛（大小写不敏感，匹配标题或摘要）；空列表表示不过滤。
    pub keywords: Vec<String>,
    /// 个人关注方向描述，注入 digest prompt 供 Codex 判断相关性。
    pub interests: String,
    pub max_results: u32,
    pub interval_secs: u64,
    pub backend: String,
    /// 终态通知路由；None 时只入队不推送。
    pub notify: Option<NotifyRoute>,
}

/// 一次轮询的结果报告。
#[derive(Debug, Serialize)]
pub struct PollReport {
    pub fetched: usize,
    pub new_entries: usize,
    pub candidates: usize,
    pub digest_task_id: Option<i64>,
}

/// arXiv 论文追踪：抓 feed → 去重 → 关键词粗筛 → 合成每日精选 digest 任务。
// TODO: 精选论文落 Vault 来源笔记（P1 Obsidian 闭环启动后接入）。
// TODO: 关键词粗筛噪音过大时接 OpenAlex/Semantic Scholar 补全（引用数、TLDR、DOI 关联）。
pub struct PaperTracker {
    store: Arc<dyn TaskStore>,
    client: ArxivClient,
    config: PapersConfig,
}

impl PaperTracker {
    pub fn new(store: Arc<dyn TaskStore>, client: ArxivClient, config: PapersConfig) -> Self {
        Self {
            store,
            client,
            config,
        }
    }

    /// 执行一轮轮询。seen 登记与 digest 任务成败解耦：抓取成功即标记，
    /// 任务失败由任务系统重试，不靠重复抓取。
    pub async fn poll_once(&self) -> Result<PollReport> {
        let entries = self
            .client
            .fetch_new(&self.config.categories, self.config.max_results)
            .await?;
        let keywords = self.config.keywords.clone();
        let interests = self.config.interests.clone();
        let outcome = crate::app::digest::register_digest(
            crate::app::digest::DigestSink {
                store: &self.store,
                seen_kind: "arxiv_seen",
                backend: &self.config.backend,
                notify: self.config.notify.as_ref(),
            },
            entries.clone(),
            crate::app::digest::DigestHooks {
                id: &|e: &ArxivEntry| e.id.clone(),
                seen_value: &|e: &ArxivEntry| {
                    serde_json::json!({
                        "version": e.version,
                        "published": e.published,
                        "first_seen": crate::util::now(),
                    })
                },
                keep: &|e: &ArxivEntry| {
                    crate::app::digest::keyword_match(
                        &keywords,
                        &format!("{} {}", e.title, e.summary),
                    )
                },
                key: &|candidates: &[ArxivEntry]| {
                    format!(
                        "arxiv:digest:{}:{}-{}",
                        today(),
                        candidates[0].id,
                        candidates.len()
                    )
                },
                prompt: &|candidates: &[ArxivEntry]| digest_prompt(&interests, candidates),
            },
        )?;
        Ok(PollReport {
            fetched: entries.len(),
            new_entries: outcome.new_items,
            candidates: outcome.candidates,
            digest_task_id: outcome.digest_task_id,
        })
    }
}

/// 当前 UTC 日期（digest 幂等键的日期段）。
fn today() -> String {
    crate::util::today_utc(crate::util::now())
}

/// 每日精选 prompt：要求 Codex 按个人方向筛选并输出结构化摘要。
fn digest_prompt(interests: &str, candidates: &[ArxivEntry]) -> String {
    let mut prompt = format!(
        "你是论文筛选助手。以下是 arXiv 最新提交的候选论文（均为摘要级信息）。\n\
         个人关注方向：{interests}\n\n\
         请筛选出与关注方向真正相关的论文（无关的直接舍弃，宁缺毋滥），对每篇输出：\n\
         标题 / arXiv 链接 / 研究问题 / 方法 / 主要结果 / 局限 / 与我的相关性（1-5 分及理由）/ 建议的下一步。\n\
         所有判断仅基于摘要，明确标注「基于摘要」，不要补写摘要中没有的实验结论。\n\
         如果全部不相关，直接说明「本批次无相关论文」。\n\n\
         == 候选论文（{} 篇）==\n",
        candidates.len()
    );
    for entry in candidates {
        prompt.push_str(&format!(
            "\n---\n[{}] {}\n作者：{}\n发布：{}\n链接：{}\n摘要：{}\n",
            entry.id,
            entry.title,
            entry.authors.join(", "),
            entry.published,
            entry.abs_url,
            entry.summary
        ));
    }
    prompt
}

impl PaperTracker {
    /// 轮询间隔（run 定时入口使用）。
    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.config.interval_secs)
    }
}

/// 按 arXiv ID 发起精读：取回元数据后合成精读任务入队，回复给用户即时反馈。
/// 幂等键 arxiv:deep:{id}，同一篇论文只精读一次；重复请求回复已有任务编号。
pub async fn deep_read_task(
    store: &Arc<dyn TaskStore>,
    client: &ArxivClient,
    id: &str,
    interests: &str,
    backend: &str,
    route: &NotifyRoute,
) -> Result<String> {
    let Some(entry) = client.fetch_by_id(id).await? else {
        return Ok(format!("未找到论文 {id}，请检查 arXiv ID"));
    };
    let result = crate::app::task::TaskService::new(store.clone()).enqueue_with_notify(
        &format!("arxiv:deep:{id}"),
        &deep_read_prompt(interests, &entry),
        backend,
        route,
    )?;
    Ok(if result.created {
        format!(
            "精读任务已创建：#{}《{}》，完成后回复你",
            result.task.id, entry.title
        )
    } else {
        format!("该论文已有精读任务：#{}", result.task.id)
    })
}

/// 精读 prompt：要求 Codex 基于摘要按固定框架深读单篇论文。
fn deep_read_prompt(interests: &str, entry: &ArxivEntry) -> String {
    format!(
        "你是论文精读助手。请对下面这篇 arXiv 论文做精读（仅有摘要级信息，不要假装读过全文）。\n\
         个人关注方向：{interests}\n\n\
         请按以下框架输出：\n\
         1. 核心问题与创新点\n\
         2. 方法细节（基于摘要可推断的部分）\n\
         3. 关键实验设计与结果\n\
         4. 局限与未说明的假设\n\
         5. 与关注方向的相关性（1-5 分及理由）\n\
         6. 可复现性线索（代码 / 数据链接，如摘要提及）\n\
         7. 值得记录的引用点\n\
         8. 建议的下一步\n\
         所有判断仅基于摘要，明确标注「基于摘要」，不要补写摘要中没有的实验结论。\n\n\
         == 论文 ==\n\
         [{}] {}\n作者：{}\n发布：{}（更新：{}）\n链接：{}\nPDF：{}\n摘要：{}\n",
        entry.id,
        entry.title,
        entry.authors.join(", "),
        entry.published,
        entry.updated,
        entry.abs_url,
        entry.pdf_url.as_deref().unwrap_or("未提供"),
        entry.summary
    )
}
