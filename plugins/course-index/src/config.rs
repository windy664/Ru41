use anyhow::Result;
use serde::Deserialize;

use crate::service::CourseIndexConfig;

/// 课程索引配置：把课表、智慧树、学习通三边的课程交叉枚举成 vault 里的课程目录与索引。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CourseIndexSection {
    /// Vault 内课程所在学期目录；默认 01-Courses-课程/2026-Fall-大一上。
    pub vault_course_dir: Option<String>,
    /// 源课程名 → Vault 课程目录名。同一门课在课表与各平台名字不一致时用它归一
    /// （例：课表「思想道德与法治」= 平台「思想道德修养与法律基础」）。
    pub course_aliases: Option<std::collections::BTreeMap<String, String>>,
    /// 同步间隔（秒），默认 3600（每小时一次）。
    pub interval_secs: Option<u64>,
}

impl CourseIndexSection {
    pub const DEFAULT_INTERVAL_SECS: u64 = 3_600;

    pub fn resolved(&self) -> Result<CourseIndexConfig> {
        Ok(CourseIndexConfig {
            vault_dir: self
                .vault_course_dir
                .clone()
                .unwrap_or_else(|| "01-Courses-课程/2026-Fall-大一上".into()),
            aliases: self.course_aliases.clone().unwrap_or_default(),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
        })
    }
}
