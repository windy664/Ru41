//! 岗位总览：抓面灵 AI 职位表（首页最新 50 条，翻页需登录所以跨轮合并
//! 状态文件扩窗口），按口径筛选（实习+校招、只留技术岗、大湾区 ⭐），
//! 生成 `05-Opportunities-机会/Internships-实习/岗位总览.md` 写进 vault，
//! 变更以 [ru41] 前缀自提交（obsidian 闭环会自认，不触发整理任务）。

pub mod filter;
pub mod overview;

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
    /// BOSS 直聘 sidecar 结果文件路径（容器内视角，如 /data/boss_jobs.json）；
    /// 缺省 None = 不启用第二数据源。
    pub boss_path: Option<String>,
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
            boss_path: config.sources.jobs.boss_path.clone(),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};

pub struct JobsService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
    boss_path: Option<String>,
}

impl JobsService {
    /// 抓面灵 tRPC 接口（实习+校招各最新 50）→ 筛选 → 合并状态 →
    /// 给没抓过 JD 的条目回源官方招聘站补 JD → 渲染 → 有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let client = crate::adapters::sources::jobs::mianling::MianlingClient::new();
        let mut jobs = client.fetch_latest("实习").await?;
        jobs.extend(client.fetch_latest("校招").await?);
        let fetched = jobs.len();
        let mut fresh = filter::filter_jobs(jobs);

        // 第二数据源：BOSS 直聘 sidecar 结果文件。读失败只降级记日志，
        // 不影响面灵链路。
        if let Some(path) = &self.boss_path {
            match crate::adapters::sources::jobs::boss::load_entries(path) {
                Ok(entries) => fresh.extend(entries),
                Err(error) => eprintln!("[jobs.boss] {path} 读取失败（降级跳过）: {error:#}"),
            }
        }

        let today = chrono::Local::now().date_naive();
        let today_md = (
            today.format("%m").to_string().parse().unwrap_or(0),
            today.format("%d").to_string().parse().unwrap_or(0),
        );

        let vault = self.vault.clone();
        let mut state: filter::JobsState = tokio::task::spawn_blocking(move || -> Result<_> {
            let mut state: filter::JobsState = vault
                .read_file(filter::STATE_PATH)?
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            filter::merge(&mut state, fresh, today_md);
            Ok(state)
        })
        .await??;

        // JD 补全：只补没试过的，单轮限 15 条，失败也标记 jd_tried 不再重试。
        let http = crate::util::http_client();
        let mut jd_filled = 0usize;
        let pending: Vec<String> = state
            .entries
            .iter()
            .filter(|(_, e)| !e.jd_tried)
            .map(|(k, _)| k.clone())
            .take(15)
            .collect();
        for key in pending {
            let entry = state.entries.get(&key).expect("pending key").clone();
            let platform = entry.platform.clone().unwrap_or_default();
            let jd = crate::adapters::sources::jobs::jd::fetch_jd(
                &http, &platform, &entry.company, &entry.role,
            )
            .await;
            let slot = state.entries.get_mut(&key).expect("pending key");
            slot.jd_tried = true;
            if let Some(text) = jd {
                slot.jd = Some(text);
                jd_filled += 1;
            }
        }

        let vault = self.vault.clone();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
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
            "jd_filled": jd_filled,
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
