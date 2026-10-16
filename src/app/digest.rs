use crate::{app::task::TaskService, domain::NotifyRoute, ports::storage::TaskStore};
use anyhow::Result;
use std::sync::Arc;

/// digest 管线的结果：去重与候选计数，以及入队的 digest 任务 id。
#[derive(Debug)]
pub struct DigestOutcome {
    pub new_items: usize,
    pub candidates: usize,
    pub digest_task_id: Option<i64>,
}

/// digest 出口：存储、去重命名空间与任务入队参数。
pub struct DigestSink<'a> {
    pub store: &'a Arc<dyn TaskStore>,
    /// records 去重命名空间（如 arxiv_seen / rss_seen）。
    pub seen_kind: &'a str,
    pub backend: &'a str,
    pub notify: Option<&'a NotifyRoute>,
}

/// 条目级钩子：去重键、登记内容、候选过滤、幂等键与 prompt 合成。
pub struct DigestHooks<'a, T> {
    /// 条目的去重键；登记发生在调用时，与 digest 任务成败解耦。
    pub id: &'a dyn Fn(&T) -> String,
    /// 登记内容（版本、首见时间等审计信息）。
    pub seen_value: &'a dyn Fn(&T) -> serde_json::Value,
    /// 候选过滤（通常是关键词粗筛）。
    pub keep: &'a dyn Fn(&T) -> bool,
    /// digest 任务幂等键与 prompt 的合成。
    pub key: &'a dyn Fn(&[T]) -> String,
    pub prompt: &'a dyn Fn(&[T]) -> String,
}

/// 来源 digest 管线：登记去重 → 过滤 → 候选非空时合成一个 digest 任务入队。
/// 抓取由各来源自行完成（协议与错误处理不同），此后的一次性逻辑统一走这里。
pub fn register_digest<T>(
    sink: DigestSink,
    items: Vec<T>,
    hooks: DigestHooks<T>,
) -> Result<DigestOutcome> {
    let mut fresh = Vec::new();
    for item in items {
        let seen_key = (hooks.id)(&item);
        if sink.store.document(sink.seen_kind, &seen_key)?.is_some() {
            continue;
        }
        sink.store
            .put_document(sink.seen_kind, &seen_key, None, &(hooks.seen_value)(&item))?;
        fresh.push(item);
    }
    let new_items = fresh.len();
    let candidates: Vec<T> = fresh
        .into_iter()
        .filter(|item| (hooks.keep)(item))
        .collect();
    let mut digest_task_id = None;
    if !candidates.is_empty() {
        let mut service = TaskService::new(sink.store.clone());
        let result = match sink.notify {
            Some(route) => service.enqueue_with_notify(
                &(hooks.key)(&candidates),
                &(hooks.prompt)(&candidates),
                sink.backend,
                route,
            )?,
            None => service.enqueue_for(
                &(hooks.key)(&candidates),
                &(hooks.prompt)(&candidates),
                sink.backend,
            )?,
        };
        digest_task_id = Some(result.task.id);
    }
    Ok(DigestOutcome {
        new_items,
        candidates: candidates.len(),
        digest_task_id,
    })
}

/// 关键词粗筛：任一关键词（大小写不敏感）命中文本即保留；空关键词表全部保留。
pub fn keyword_match(keywords: &[String], text: &str) -> bool {
    if keywords.is_empty() {
        return true;
    }
    let haystack = text.to_lowercase();
    keywords
        .iter()
        .any(|k| haystack.contains(&k.to_lowercase()))
}
