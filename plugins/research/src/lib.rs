//! 科研机会总览：校外可个人申请的暑研/科研实习计划（Mitacs、港校暑研、
//! 中科院科创等）。这些项目没有公开 API，窗口一年一次错过等一年——
//! 本模块渲染带窗口倒计时的常驻总览到
//! `05-Opportunities-机会/Research-科研/科研总览.md`（纯本地渲染，零网络依赖），
//! 真实开窗由 pagewatch 盯官网页面变更兜底。
//! 与 programs（GSoC/LFX/OSPP 开源项目计划）的区别：科研 = 学术研究经历。

mod overview;

pub use overview::ResearchProgram;

use ru41::ports::vault::Vault;
use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;

/// 总览在 vault 里的位置。
pub const OVERVIEW_PATH: &str = "05-Opportunities-机会/Research-科研/科研总览.md";

/// `[sources.research]`：科研机会总览附属。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResearchSection {
    /// 生成间隔（秒），默认 6 小时。
    pub interval_secs: Option<u64>,
}

/// 注册表定义：依赖 obsidian 的 vault 配置（同一个仓库），未配置则缺席。
pub static DEF: ru41::modules::ModuleDef = ru41::modules::ModuleDef {
    name: "research",
    priority: ru41::modules::DEFAULT_PRIORITY,
    config_key: Some("research"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![ru41::modules::JobSchedule::simple(
            "research",
            config
                .sources
                .get_or_default::<crate::ResearchSection>("research")
                .interval_secs
                .unwrap_or(6 * 3600),
            ready,
            (!ready).then(|| "复用 [obsidian] vault_path，未配置".into()),
        )]
    },
    tracker: |config, _store, name| {
        if name != "research" {
            return Ok(None);
        }
        let resolved = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(ResearchService {
            vault: ru41::bootstrap::assembly::vault_client(&resolved),
            interval_secs: config
                .sources
                .get::<crate::ResearchSection>("research")?
                .interval_secs
                .unwrap_or(6 * 3600),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    image_consumers: ru41::modules::no_image_consumers,
    cli: ru41::modules::no_cli,
};

#[derive(Clone)]
pub struct ResearchService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
}

impl ResearchService {
    /// 纯渲染（无网络抓取）：窗口倒计时随日期变化，有变化才写文件并自提交。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let content = overview::render(chrono::Local::now().date_naive());
        let vault = self.vault.clone();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
            if vault.read_file(OVERVIEW_PATH)?.as_deref() == Some(content.as_str()) {
                return Ok(false);
            }
            vault.write_file(OVERVIEW_PATH, &content)?;
            if vault.commit_all("[ru41] 科研总览")? {
                vault.push()?;
            }
            Ok(true)
        })
        .await??;
        Ok(serde_json::json!({
            "action": if changed { "updated" } else { "unchanged" },
        }))
    }
}

impl ru41::ingress::schedule::PollJob for ResearchService {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        let owned = self.clone();
        ru41::plugins::bridge(async move { owned.poll_once().await })
    }
}

/// 宿主加载入口（插件不用 linkme，走显式注册）。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {
    ru41::plugins::PluginEntry::new("research", || DEF)
}
