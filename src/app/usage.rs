//! token 用量聚合：按 request_key 前缀（即来源插件/入口）分组统计。
//!
//! 数据来自 attempts 表里底座事件流上报的 usage（storage::usage_since）；
//! 底座未上报用量的运行不参与统计。

use crate::ports::storage::{TaskStore, UsageRecord};
use anyhow::Result;

/// 单个来源的用量小计。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageSummary {
    /// request_key 的第一个 `:` 前缀（如 `lecture:notes:x` → `lecture`）。
    pub source: String,
    pub runs: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 单个模型的用量小计：换供应方/换模型后，用量要能按模型分摊。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModelUsage {
    /// 实际使用的模型（底座 + 供应方组合，如 minimax/MiniMax-M3）。
    pub model: String,
    pub runs: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

pub fn source_of(request_key: &str) -> &str {
    request_key.split(':').next().unwrap_or(request_key)
}

/// 按来源分组小计；来源名按字典序输出，保证展示稳定。
pub fn summarize(records: &[UsageRecord]) -> Vec<UsageSummary> {
    let mut map: std::collections::BTreeMap<&str, UsageSummary> = std::collections::BTreeMap::new();
    for record in records {
        let source = source_of(&record.request_key);
        let entry = map.entry(source).or_insert_with(|| UsageSummary {
            source: source.to_owned(),
            runs: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        });
        entry.runs += 1;
        entry.input_tokens += record.input_tokens as u64;
        entry.cached_input_tokens += record.cached_input_tokens as u64;
        entry.output_tokens += record.output_tokens as u64;
        entry.total_tokens += record.total_tokens as u64;
    }
    map.into_values().collect()
}

/// 按实际使用的模型分组小计；模型名按字典序输出。
/// 未记录模型的运行归到「未知模型」，不静默丢弃。
pub fn summarize_by_model(records: &[UsageRecord]) -> Vec<ModelUsage> {
    let mut map: std::collections::BTreeMap<String, ModelUsage> = std::collections::BTreeMap::new();
    for record in records {
        let model = record
            .model
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "未知模型".to_owned());
        let entry = map.entry(model.clone()).or_insert_with(|| ModelUsage {
            model,
            runs: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        });
        entry.runs += 1;
        entry.input_tokens += record.input_tokens as u64;
        entry.cached_input_tokens += record.cached_input_tokens as u64;
        entry.output_tokens += record.output_tokens as u64;
        entry.total_tokens += record.total_tokens as u64;
    }
    map.into_values().collect()
}

/// 汇总 `since`（unix 秒）以来的用量。
pub fn report(store: &dyn TaskStore, since: i64) -> Result<Vec<UsageSummary>> {
    Ok(summarize(&store.usage_since(since)?))
}

/// 汇总 `since`（unix 秒）以来按实际模型分摊的用量。
pub fn report_by_model(store: &dyn TaskStore, since: i64) -> Result<Vec<ModelUsage>> {
    Ok(summarize_by_model(&store.usage_since(since)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(request_key: &str, total: u64) -> UsageRecord {
        UsageRecord {
            task_id: 1,
            attempt_id: 1,
            request_key: request_key.to_owned(),
            started_at: 0,
            model: Some("test/model".to_owned()),
            input_tokens: (total / 2) as i64,
            cached_input_tokens: 0,
            output_tokens: (total - total / 2) as i64,
            total_tokens: total as i64,
        }
    }

    #[test]
    fn groups_by_request_key_prefix() {
        let records = vec![
            record("lecture:notes:1", 100),
            record("lecture:notes:2", 50),
            record("qq:msg:3", 30),
        ];
        let summaries = summarize(&records);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].source, "lecture");
        assert_eq!(summaries[0].runs, 2);
        assert_eq!(summaries[0].total_tokens, 150);
        assert_eq!(summaries[1].source, "qq");
    }

    #[test]
    fn key_without_prefix_stays_whole() {
        let summaries = summarize(&[record("adhoc-task", 10)]);
        assert_eq!(summaries[0].source, "adhoc-task");
    }

    #[test]
    fn groups_by_model() {
        let mut first = record("qq:msg:1", 100);
        let mut second = record("qq:msg:2", 50);
        first.model = Some("minimax/MiniMax-M3".to_owned());
        second.model = Some("minimax/MiniMax-M3".to_owned());
        let mut third = record("lecture:notes:3", 30);
        third.model = Some("zhipu/glm-4.6".to_owned());
        let mut unknown = record("qq:msg:4", 10);
        unknown.model = None;
        let models = summarize_by_model(&[first, second, third, unknown]);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].model, "minimax/MiniMax-M3");
        assert_eq!(models[0].runs, 2);
        assert_eq!(models[0].total_tokens, 150);
        // BTreeMap 按字符串排序：ASCII 模型名在前，中文名在后
        assert_eq!(models[1].model, "zhipu/glm-4.6");
        assert_eq!(models[2].model, "未知模型");
        assert_eq!(models[2].total_tokens, 10);
    }
}
