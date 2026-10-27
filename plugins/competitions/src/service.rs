//! 竞赛日程总览：把机会雷达的竞赛源（HackerTrip / Devpost / wqai）抓到的
//! 赛事按统一口径筛选（必须有奖、大湾区线下或线上、未截止、非中小学生赛），
//! 每天生成 `05-Opportunities-机会/Competitions-竞赛/日程总览.md` 写进 vault，
//! 变更以 [ru41] 前缀自提交（obsidian 闭环会自认，不触发整理任务）。


pub use crate::filter::{CompetitionRow, build_rows};

use ru41::ports::vault::Vault;
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
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "competitions",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("competitions"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "competitions",
            config
                .sources
                .get_or_default::<CompetitionsSection>("competitions")
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
            vault: ru41::bootstrap::assembly::vault_client(&resolved),
            interval_secs: config
                .sources
                .get::<CompetitionsSection>("competitions")?
                .interval_secs
                .unwrap_or(6 * 3600),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
};

#[derive(Clone)]
pub struct CompetitionsService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
}

impl CompetitionsService {
    /// 抓全部竞赛源 → 筛选 → 渲染 → 有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let rows = self.collect_rows().await;
        let content = crate::overview::render(&rows, chrono::Local::now().date_naive());
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

        let ht = ru41::adapters::sources::competition::hackertrip::HackerTripClient::new();
        match ht
            .fetch_events("https://hackertrip.space/api/hackathons")
            .await
        {
            Ok(events) => all.extend(events.into_iter().filter_map(crate::filter::collect_hackertrip)),
            Err(error) => eprintln!("[competitions] hackertrip fetch failed: {error:#}"),
        }

        let dp = ru41::adapters::sources::competition::devpost::DevpostClient::new();
        match dp
            .fetch_all("https://devpost.com/api/hackathons?status%5B%5D=upcoming&status%5B%5D=open&order_by=deadline")
            .await
        {
            Ok(events) => all.extend(events.iter().filter_map(crate::filter::collect_devpost)),
            Err(error) => eprintln!("[competitions] devpost fetch failed: {error:#}"),
        }

        let wqai = ru41::adapters::sources::competition::wqai::WqaiClient::new();
        match wqai
            .fetch_races("https://wqai.ltd/calendar/race/index.html")
            .await
        {
            Ok(races) => {
                let today = chrono::Local::now().date_naive();
                all.extend(
                    races
                        .into_iter()
                        .filter_map(|race| crate::filter::collect_wqai(race, today)),
                );
            }
            Err(error) => eprintln!("[competitions] wqai fetch failed: {error:#}"),
        }

        let h123 = ru41::adapters::sources::competition::hack123::Hack123Client::new();
        match h123
            .fetch_events("https://hack123.henryht.sh/llms.txt")
            .await
        {
            Ok(events) => all.extend(events.iter().filter_map(crate::filter::collect_hack123)),
            Err(error) => eprintln!("[competitions] hack123 fetch failed: {error:#}"),
        }

        // 国内直采源：讯飞（浏览器渲染，站方无公开 API）+ DataFountain（SSR 列表页）。
        let xfyun = ru41::adapters::sources::competition::xfyun::XfyunClient::new();
        match xfyun
            .fetch_competitions("https://challenge.xfyun.cn/competition")
            .await
        {
            Ok(items) => all.extend(items.iter().filter_map(crate::filter::collect_xfyun)),
            Err(error) => eprintln!("[competitions] xfyun fetch failed: {error:#}"),
        }

        let df = ru41::adapters::sources::competition::datafountain::DataFountainClient::new();
        match df
            .fetch_competitions("https://www.datafountain.cn/competitions")
            .await
        {
            Ok(items) => all.extend(items.iter().filter_map(crate::filter::collect_datafountain)),
            Err(error) => eprintln!("[competitions] datafountain fetch failed: {error:#}"),
        }

        // CTFtime：全球 CTF 日历（国内 S/A 级安全赛赛季在此登记），未来 120 天窗口。
        let ctf = ru41::adapters::sources::competition::ctftime::CtftimeClient::new();
        match ctf.fetch_events(120).await {
            Ok(events) => all.extend(events.iter().filter_map(crate::filter::collect_ctftime)),
            Err(error) => eprintln!("[competitions] ctftime fetch failed: {error:#}"),
        }

        let today = chrono::Local::now().date_naive();
        build_rows(all, today)
    }
}

impl ru41::ingress::schedule::PollJob for CompetitionsService {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        // 插件态：真实工作必须在插件自己的 tokio runtime 上跑（dylib 各自
        // 静态链接 tokio，宿主 reactor 对本插件代码不可见）；桥内 catch_unwind。
        let owned = self.clone();
        ru41::plugins::bridge(async move { owned.poll_once().await })
    }

}
