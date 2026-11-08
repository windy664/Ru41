//! 活动日程总览附属：茶思屋会议日历 → 筛选（科技向 + 大湾区⭐/全国大会标记）→
//! `05-Opportunities-机会/Events-活动/活动总览.md` 写进 vault（[ru41] 自提交）。
//! 不推送通知——活动信息只沉淀到 Obsidian，用户自己翻（2026-11-01 用户定）。

pub use crate::filter::{EventRow, build_rows};
pub use crate::source::ChasparkClient;

use anyhow::Result;
use ru41::app::obsidian::ObsidianSectionExt;
use ru41::ports::vault::Vault;
use serde::Deserialize;
use std::sync::Arc;

/// 总览在 vault 里的位置。
pub const OVERVIEW_PATH: &str = "05-Opportunities-机会/Events-活动/活动总览.md";

/// `[sources.events]`：活动日程总览附属。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EventsSection {
    /// 生成间隔（秒），默认 6 小时。
    pub interval_secs: Option<u64>,
}

#[derive(Clone)]
pub struct EventsService {
    vault: Arc<dyn Vault>,
    client: ChasparkClient,
    interval_secs: u64,
}

impl EventsService {
    pub fn new(vault: Arc<dyn Vault>, interval_secs: u64) -> Self {
        Self {
            vault,
            client: ChasparkClient::new(),
            interval_secs,
        }
    }

    /// 抓会议日历 → 筛选 → 渲染 → 有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let now = ru41::util::now();
        let rows = self.collect_rows(now).await;
        let content = crate::overview::render(&rows, chrono::Local::now().date_naive());
        let vault = self.vault.clone();
        let count = rows.len();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
            if vault.read_file(OVERVIEW_PATH)?.as_deref() == Some(content.as_str()) {
                return Ok(false);
            }
            vault.write_file(OVERVIEW_PATH, &content)?;
            if vault.commit_all("[ru41] 活动日程总览")? {
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

    async fn collect_rows(&self, now: i64) -> Vec<EventRow> {
        let mut all = Vec::new();
        // 茶思屋会议日历：昨天起 30 天（昨天的留一天便于跨天查看）。
        match self
            .client
            .fetch_calendar(now - 86400, now + 30 * 86400)
            .await
        {
            Ok(items) => all.extend(items),
            Err(error) => eprintln!("[events] 茶思屋日历抓取失败: {error:#}"),
        }
        // 活动行：大湾区三城民间活动（一页即最近，不翻页）。
        let hdx = crate::source::HuodongxingClient::new();
        for city in crate::source::HDX_CITIES {
            match hdx.fetch_city(city).await {
                Ok(items) => all.extend(items),
                Err(error) => eprintln!("[events] 活动行 {city} 抓取失败: {error:#}"),
            }
        }
        build_rows(all, now)
    }
}

impl ru41::ingress::schedule::SimplePollJob for EventsService {
    type Report = serde_json::Value;

    fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        self.poll_once().await
    }
}

/// 注册表定义：依赖 obsidian vault 配置（未配置则缺席）。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "events",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("events"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "events",
            config
                .sources
                .get_or_default::<EventsSection>("events")
                .interval_secs
                .unwrap_or(6 * 3600),
            ready,
            (!ready).then(|| "复用 [obsidian] vault_path，未配置".into()),
        )]
    },
    tracker: |config, _store, name| {
        if name != "events" {
            return Ok(None);
        }
        let section = config.sources.get::<EventsSection>("events")?;
        let resolved = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(EventsService::new(
            ru41::bootstrap::assembly::vault_client(&resolved),
            section.interval_secs.unwrap_or(6 * 3600),
        ))))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};
