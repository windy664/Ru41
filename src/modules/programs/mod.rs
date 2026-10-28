//! 项目计划总览：GSoC 官方 API 时间线 + LFX Mentorship 开放申请项目 +
//! OSPP 开源之夏（无公开 API，静态卡列往年节奏提示关注窗口），生成
//! `05-Opportunities-机会/Programs-项目计划/项目总览.md` 写进 vault，
//! 变更以 [ru41] 前缀自提交（obsidian 闭环会自认，不触发整理任务）。
//! 雷达池里的 gsoc+/lfx+ 源负责"到点提醒"，本模块负责 vault 里的常驻全景。

mod overview;

use crate::ports::vault::Vault;
use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;

/// 总览在 vault 里的位置。
pub const OVERVIEW_PATH: &str = "05-Opportunities-机会/Programs-项目计划/项目总览.md";

/// `[sources.programs]`：项目计划总览附属。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProgramsSection {
    /// 生成间隔（秒），默认 6 小时。
    pub interval_secs: Option<u64>,
}

/// 注册表定义：依赖 obsidian 的 vault 配置（同一个仓库），未配置则缺席。
#[linkme::distributed_slice(super::MODULES)]
pub static DEF: super::ModuleDef = super::ModuleDef {
    name: "programs",
    priority: super::DEFAULT_PRIORITY,
    config_key: Some("programs"),
    schedule: |config| {
        let ready = config.obsidian.is_ready();
        vec![super::JobSchedule::simple(
            "programs",
            config
                .sources
                .get_or_default::<crate::modules::programs::ProgramsSection>("programs")
                .interval_secs
                .unwrap_or(6 * 3600),
            ready,
            (!ready).then(|| "复用 [obsidian] vault_path，未配置".into()),
        )]
    },
    tracker: |config, _store, name| {
        if name != "programs" {
            return Ok(None);
        }
        let resolved = config.obsidian.resolved(None)?;
        Ok(Some(Box::new(ProgramsService {
            vault: crate::bootstrap::assembly::vault_client(&resolved),
            interval_secs: config
                .sources
                .get::<crate::modules::programs::ProgramsSection>("programs")?
                .interval_secs
                .unwrap_or(6 * 3600),
        })))
    },
    qq_handlers: |_, _| Ok(Vec::new()),
    qq_image_consumers: |_| Ok(Vec::new()),
};

pub struct ProgramsService {
    vault: Arc<dyn Vault>,
    interval_secs: u64,
}

impl ProgramsService {
    /// 抓 GSoC 当季时间线 + LFX 开放项目 → 渲染 → 有变化才写文件并自提交。
    /// 单源失败记日志降级，不挡其他源。
    pub async fn poll_once(&self) -> Result<serde_json::Value> {
        let gsoc = match crate::adapters::sources::competition::gsoc::GsocClient::new()
            .fetch_active_program()
            .await
        {
            Ok(program) => program,
            Err(error) => {
                eprintln!("[programs] gsoc fetch failed: {error:#}");
                None
            }
        };
        let lfx = match crate::adapters::sources::competition::lfx::LfxClient::new()
            .fetch_projects("https://api.mentorship.lfx.linuxfoundation.org/projects")
            .await
        {
            Ok(items) => items,
            Err(error) => {
                eprintln!("[programs] lfx fetch failed: {error:#}");
                Vec::new()
            }
        };

        let content = overview::render(gsoc.as_ref(), &lfx, chrono::Local::now().date_naive());
        let gsoc_milestones = gsoc.as_ref().map(|p| p.milestones.len()).unwrap_or(0);
        let lfx_open = lfx.len();

        let vault = self.vault.clone();
        let changed = tokio::task::spawn_blocking(move || -> Result<bool> {
            if vault.read_file(OVERVIEW_PATH)?.as_deref() == Some(content.as_str()) {
                return Ok(false);
            }
            vault.write_file(OVERVIEW_PATH, &content)?;
            if vault.commit_all("[ru41] 项目总览")? {
                vault.push()?;
            }
            Ok(true)
        })
        .await??;
        Ok(serde_json::json!({
            "action": if changed { "updated" } else { "unchanged" },
            "gsoc_milestones": gsoc_milestones,
            "lfx_open": lfx_open,
        }))
    }
}

impl crate::ingress::schedule::PollJob for ProgramsService {
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
