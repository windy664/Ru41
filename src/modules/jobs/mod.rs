//! 岗位总览：抓面灵 AI 职位表（首页最新 50 条，翻页需登录所以跨轮合并
//! 状态文件扩窗口），按口径筛选（实习+校招、只留技术岗、大湾区 ⭐），
//! 生成 `05-Opportunities-机会/Internships-实习/岗位总览.md` 写进 vault，
//! 变更以 [ru41] 前缀自提交（obsidian 闭环会自认，不触发整理任务）。

mod filter;
mod overview;

use crate::ports::vault::Vault;
use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;

/// 总览在 vault 里的位置。
pub const OVERVIEW_PATH: &str = "05-Opportunities-机会/Internships-实习/岗位总览.md";

/// `[sources.jobs]`：岗位总览附属。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsSection {
    /// 生成间隔（秒），默认 6 小时。
    pub interval_secs: Option<u64>,
}

/// 注册表定义：依赖 obsidian 的 vault 配置（同一个仓库），未配置则缺席。
pub const DEF: super::ModuleDef = super::ModuleDef {
    name: "jobs",
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
            "jobs",
            config.sources.jobs.interval_secs.unwrap_or(6 * 3600),
            ready,
            (!ready).then(|| "复用 [obsidian] vault_path，未配置".into()),
        )]
    },
    tracker: |config, _store, name| {
        if name != "jobs" {
            return Ok(None);
        }
        let resolved = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(JobsService {
            vault: crate::bootstrap::assembly::vault_client(&resolved),
            interval_secs: config.sources.jobs.interval_secs.unwrap_or(6 * 3600),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};

pub struct JobsService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
}

impl JobsService {
    /// 抓面灵 → 筛选 → 合并状态 → 渲染 → 有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let client = crate::adapters::sources::jobs::mianling::MianlingClient::new();
        let jobs = client
            .fetch_jobs_struct("https://www.mianlingai.com/jobs/")
            .await?;
        let fetched = jobs.len();
        let fresh = filter::filter_jobs(jobs);

        let today = chrono::Local::now().date_naive();
        let today_md = (today.format("%m").to_string().parse().unwrap_or(0), today.format("%d").to_string().parse().unwrap_or(0));

        let vault = self.vault.clone();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
            let mut state: filter::JobsState = vault
                .read_file(filter::STATE_PATH)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            filter::merge(&mut state, fresh, today_md);
            let rows: Vec<filter::JobEntry> = state.entries.values().cloned().collect();
            let content = overview::render(&rows, today);

            let old_overview = vault.read_file(OVERVIEW_PATH)?;
            let old_state = vault.read_file(filter::STATE_PATH)?;
            let state_json = serde_json::to_string_pretty(&state)?;
            if old_overview.as_deref() == Some(content.as_str())
                && old_state.as_deref() == Some(state_json.as_str())
            {
                return Ok(false);
            }
            vault.write_file(filter::STATE_PATH, &state_json)?;
            vault.write_file(OVERVIEW_PATH, &content)?;
            if vault.commit_all("[ru41] 岗位总览")? {
                vault.push()?;
            }
            Ok(true)
        })
        .await??;
        Ok(serde_json::json!({
            "action": if changed { "updated" } else { "unchanged" },
            "fetched": fetched,
        }))
    }
}

impl crate::ingress::schedule::PollJob for JobsService {
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
