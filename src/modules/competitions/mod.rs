//! 竞赛日程总览：把机会雷达的竞赛源（HackerTrip / Devpost / wqai）抓到的
//! 赛事按统一口径筛选（必须有奖、大湾区线下或线上、未截止、非中小学生赛），
//! 每天生成 `05-Opportunities-机会/Competitions-竞赛/日程总览.md` 写进 vault，
//! 变更以 [ru41] 前缀自提交（obsidian 闭环会自认，不触发整理任务）。

mod filter;
mod overview;

pub use filter::{CompetitionRow, build_rows};

use crate::ports::vault::Vault;
use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;

/// 总览在 vault 里的位置。
pub const OVERVIEW_PATH: &str = "05-Opportunities-机会/Competitions-竞赛/日程总览.md";

/// `[sources.competitions]`：竞赛日程总览附属。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompetitionsSection {
    /// 生成间隔（秒），默认 6 小时。
    pub interval_secs: Option<u64>,
}

/// 注册表定义：依赖 obsidian 的 vault 配置（同一个仓库），未配置则缺席。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "competitions",
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
            "competitions",
            config
                .sources
                .competitions
                .interval_secs
                .unwrap_or(6 * 3600),
            ready,
            (!ready).then(|| "复用 [obsidian] vault_path，未配置".into()),
        )]
    },
    tracker: |config, _store, name| {
        if name != "competitions" {
            return Ok(None);
        }
        let resolved = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(CompetitionsService {
            vault: crate::bootstrap::assembly::vault_client(&resolved),
            interval_secs: config
                .sources
                .competitions
                .interval_secs
                .unwrap_or(6 * 3600),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};

pub struct CompetitionsService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
}

impl CompetitionsService {
    /// 抓全部竞赛源 → 筛选 → 渲染 → 有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let rows = self.collect_rows().await;
        let content = overview::render(&rows, chrono::Local::now().date_naive());
        let vault = self.vault.clone();
        let count = rows.len();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
            if vault.read_file(OVERVIEW_PATH)?.as_deref() == Some(content.as_str()) {
                return Ok(false);
            }
            vault.write_file(OVERVIEW_PATH, &content)?;
            if vault.commit_all("[ru41] 竞赛日程总览")? {
                vault.push()?;
            }
            Ok(true)
        })
        .await??;
        Ok(serde_json::json!({
            "action": if changed { "updated" } else { "unchanged" },
            "rows": count,
        }))
    }

    /// 逐源抓取；单源失败记日志降级，不挡其他源。
    async fn collect_rows(&self) -> Vec<CompetitionRow> {
        let mut all = Vec::new();

        let ht = crate::adapters::sources::competition::hackertrip::HackerTripClient::new();
        match ht
            .fetch_events("https://hackertrip.space/api/hackathons")
            .await
        {
            Ok(events) => all.extend(events.into_iter().filter_map(filter::collect_hackertrip)),
            Err(error) => eprintln!("[competitions] hackertrip fetch failed: {error:#}"),
        }

        let dp = crate::adapters::sources::competition::devpost::DevpostClient::new();
        match dp
            .fetch_all("https://devpost.com/api/hackathons?status%5B%5D=upcoming&status%5B%5D=open&order_by=deadline")
            .await
        {
            Ok(events) => all.extend(events.iter().filter_map(filter::collect_devpost)),
            Err(error) => eprintln!("[competitions] devpost fetch failed: {error:#}"),
        }

        let wqai = crate::adapters::sources::competition::wqai::WqaiClient::new();
        match wqai
            .fetch_races("https://wqai.ltd/calendar/race/index.html")
            .await
        {
            Ok(races) => {
                let today = chrono::Local::now().date_naive();
                all.extend(
                    races
                        .into_iter()
                        .filter_map(|race| filter::collect_wqai(race, today)),
                );
            }
            Err(error) => eprintln!("[competitions] wqai fetch failed: {error:#}"),
        }

        let today = chrono::Local::now().date_naive();
        build_rows(all, today)
    }
}

impl crate::ingress::schedule::PollJob for CompetitionsService {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move { self.poll_once().await })
    }
}
