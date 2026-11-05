//! token 用量聚合：按 request_key 前缀（即来源插件/入口）分组统计。
//!
//! 数据来自 attempts 表里底座事件流上报的 usage（storage::usage_since）；
//! 底座未上报用量的运行不参与统计。

use crate::ports::storage::{TaskStore, UsageRecord};
use anyhow::Result;
use std::collections::BTreeMap;

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

/// 一次分组小计（模型 / 来源 / 天）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageStat {
    /// 分组名（模型 ID / 来源前缀 / 日期）。
    pub key: String,
    /// 该组内的运行总次数（含未上报用量的）。
    pub runs: u32,
    /// 其中上报了用量的次数。
    pub reported_runs: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl UsageStat {
    /// 该组是否完全没有 token 数据（底座未上报）。
    pub fn unreported(&self) -> bool {
        self.reported_runs == 0
    }
}

/// 用量总览：以"跑了多少次"为主指标，token 为副指标。
///
/// 之前只统计上报过 token 的运行，于是底座不上报用量时（codex 走 GLM 的 chat
/// 接口不返回 usage）报告几乎空白——近 7 天 123 次运行里只有 1 次有数字。
/// 现在把运行次数、覆盖率、分来源/模型/天都列出来，空不空一眼可见。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UsageOverview {
    /// 统计窗口（天）。
    pub days: u32,
    pub runs: u32,
    pub reported_runs: u32,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// 按模型分组（用量多的在前）。
    pub by_model: Vec<UsageStat>,
    /// 按功能来源分组（用量多的在前）。
    pub by_source: Vec<UsageStat>,
    /// 按天分组（最近的在前）。
    pub by_day: Vec<UsageStat>,
}

/// 从窗口内的全部运行算出总览。
pub fn overview(runs: &[crate::ports::storage::UsageRun], days: u32) -> UsageOverview {
    let mut models: BTreeMap<String, UsageStat> = BTreeMap::new();
    let mut sources: BTreeMap<String, UsageStat> = BTreeMap::new();
    let mut days_map: BTreeMap<String, UsageStat> = BTreeMap::new();
    let mut totals = UsageStat {
        key: String::new(),
        runs: 0,
        reported_runs: 0,
        input_tokens: 0,
        cached_input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
    };
    for run in runs {
        // 模型名带底座前缀（codex/glm-4.6、pi/MiniMax-M3），免得同名不同底座看不清。
        let model = match run.model.clone() {
            Some(model) if model.contains('/') => model,
            Some(model) => format!("{}/{model}", run.backend_id),
            None => format!("{}/（未记录模型）", run.backend_id),
        };
        let source = source_of(&run.request_key).to_owned();
        let day = day_label(run.started_at);
        let reported = run.total_tokens.is_some();
        let (input, cached, output, total) = match run.total_tokens {
            Some(total) => (
                run.input_tokens.unwrap_or(0).max(0) as u64,
                run.cached_input_tokens.unwrap_or(0).max(0) as u64,
                run.output_tokens.unwrap_or(0).max(0) as u64,
                total.max(0) as u64,
            ),
            None => (0, 0, 0, 0),
        };
        for (map, key) in [
            (&mut models, model.clone()),
            (&mut sources, source.clone()),
            (&mut days_map, day.clone()),
        ] {
            let entry = map.entry(key.clone()).or_insert_with(|| UsageStat {
                key: key.clone(),
                runs: 0,
                reported_runs: 0,
                input_tokens: 0,
                cached_input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });
            entry.runs += 1;
            if reported {
                entry.reported_runs += 1;
                entry.input_tokens += input;
                entry.cached_input_tokens += cached;
                entry.output_tokens += output;
                entry.total_tokens += total;
            }
        }
        totals.runs += 1;
        if reported {
            totals.reported_runs += 1;
            totals.input_tokens += input;
            totals.cached_input_tokens += cached;
            totals.output_tokens += output;
            totals.total_tokens += total;
        }
    }
    let by_sort = |mut items: Vec<UsageStat>| {
        items.sort_by(|a, b| {
            b.reported_runs
                .cmp(&a.reported_runs)
                .then(b.runs.cmp(&a.runs))
                .then(a.key.cmp(&b.key))
        });
        items
    };
    UsageOverview {
        days,
        runs: totals.runs,
        reported_runs: totals.reported_runs,
        input_tokens: totals.input_tokens,
        cached_input_tokens: totals.cached_input_tokens,
        output_tokens: totals.output_tokens,
        total_tokens: totals.total_tokens,
        by_model: by_sort(models.into_values().collect()),
        by_source: by_sort(sources.into_values().collect()),
        by_day: {
            let mut items: Vec<UsageStat> = days_map.into_values().collect();
            items.sort_by(|a, b| b.key.cmp(&a.key));
            items
        },
    }
}

/// 本地日期标签（东八区，服务器时区即本地时区）。
fn day_label(started_at: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(started_at, 0)
        .single()
        .map(|t| t.format("%m-%d").to_string())
        .unwrap_or_default()
}

/// 来源前缀的中文说明（request_key 的第一段）。
pub fn source_label(source: &str) -> &'static str {
    match source {
        "qq" => "QQ 消息",
        "lecture" => "课堂录音整理",
        "rss" => "RSS 日报",
        "homework" => "作业提醒",
        "chaoxing" => "学习通作业",
        "zhihuishu" => "智慧树作业",
        "jwxt" => "教务课表",
        "library" => "图书馆",
        "ecard" => "水电",
        "plan" | "planner" => "行程",
        "obsidian" => "知识库整理",
        "digest" => "每日简报",
        _ => "",
    }
}
