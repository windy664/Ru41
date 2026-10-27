use anyhow::Result;
use serde::Deserialize;

use crate::service::ProjectsConfig;

/// 项目库管理配置；projects poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectsSection {
    /// Vault 内项目库根目录；默认 02-Projects-项目。
    pub projects_dir: Option<String>,
    /// GitHub 同步轮询间隔（秒），默认 86400（每日）；README 变化才覆写。
    pub interval_secs: Option<u64>,
    /// GitHub 账号：配置后每日拉取该账号全部公开仓库，各建项目目录并同步 README。
    pub github_user: Option<String>,
    /// 仓库最后更新超过该天数自动归入 Archived-已归档；默认 14（两周没动即归档）。
    pub archive_after_days: Option<u64>,
    /// 同步发现 README 变更/元数据变化时是否经默认通知出口推送；默认 false
    /// （低频归档型任务，默认静默，变更会在下一次 QQ /项目 或巡检中可见）。
    pub notify: Option<bool>,
}

impl ProjectsSection {
    /// run 的轮询间隔（秒）默认值。
    pub const DEFAULT_INTERVAL_SECS: u64 = 86_400;

    /// 由 [sources.projects] 解析出运行时配置。
    pub fn resolved(&self, vault_root: std::path::PathBuf) -> Result<ProjectsConfig> {
        Ok(ProjectsConfig {
            vault_root,
            projects_dir: self
                .projects_dir
                .clone()
                .unwrap_or_else(|| "02-Projects-项目".into()),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            github_user: self.github_user.clone(),
            archive_after_days: self.archive_after_days.unwrap_or(14),
        })
    }
}
