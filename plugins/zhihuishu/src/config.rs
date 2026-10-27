use anyhow::Result;
use serde::Deserialize;

use crate::service::ZhihuishuConfig;

/// 智慧树作业提醒配置；zhihuishu poll/run 使用。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ZhihuishuSection {
    /// sidecar 采集器写出的报告文件路径；默认 .ru41/zhihuishu/report.json。
    pub report_path: Option<String>,
    /// 截止前 N 天开始每日提醒，默认 3。
    pub deadline_days: Option<i64>,
    /// zhihuishu run 的轮询间隔（秒），默认 21600（6 小时）。
    pub interval_secs: Option<u64>,
    /// 新任务与临近截止是否经默认通知出口推送（[notify] channel）；默认 true。
    pub notify: Option<bool>,
    /// 是否把未完成作业自动写进待办（todo_item），平台完成时自动勾选；默认 true。
    pub todo_sync: Option<bool>,
    /// 是否把作业清单同步进 Obsidian Vault 的课程目录（需 [obsidian] 已配置）；默认 true。
    pub vault_sync: Option<bool>,
    /// Vault 内课程所在学期目录；默认 01-Courses-课程/2026-Fall-大一上。
    pub vault_course_dir: Option<String>,
    /// 智慧树课程名 → Vault 课程目录名映射；缺省用内置的当前学期映射，未映射课程按原名建目录。
    #[serde(default)]
    pub vault_courses: Option<std::collections::BTreeMap<String, String>>,
    /// 任务目录策略："all"（默认，每个任务都建目录）或 "substantive"
    /// （只有实质作业——有正文要求或附件——才建目录，纯在线任务不进 vault）。
    pub vault_task_dirs: Option<String>,
}

impl ZhihuishuSection {
    /// run 的轮询间隔（秒）默认值；tracker 装配与 schedule list 共用这一份。
    pub const DEFAULT_INTERVAL_SECS: u64 = 21_600;
    /// sidecar 报告路径默认值。
    /// 智慧树课程名 → Vault 目录名的内置映射（按 2026 秋实际课程核对）。
    pub const DEFAULT_COURSE_MAP: &[(&str, &str)] = &[
        ("26-27-1思想道德与法治", "思想道德修养与法律基础"),
        ("26-27（1）思想道德与法治", "思想道德修养与法律基础"),
        ("职业生涯规划", "大学生职业生涯规划"),
        ("网络空间安全导论", "网络空间安全导论"),
        ("高等数学I(理)", "高等数学"),
        ("军事理论", "军事理论"),
        ("国家安全教育", "国家安全教育"),
        ("形势与政策-2026-2027-1", "形势与政策"),
    ];
    pub const DEFAULT_REPORT_PATH: &str = ".ru41/zhihuishu/report.json";

    /// 由 [sources.zhihuishu] 解析出运行时配置；默认值只定义在这里。
    pub fn resolved(&self, notify_target: String) -> Result<ZhihuishuConfig> {
        let vault_courses: std::collections::BTreeMap<String, String> =
            self.vault_courses.clone().unwrap_or_else(|| {
                Self::DEFAULT_COURSE_MAP
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            });
        Ok(ZhihuishuConfig {
            report_path: self
                .report_path
                .clone()
                .unwrap_or_else(|| Self::DEFAULT_REPORT_PATH.into())
                .into(),
            attachments_dir: {
                // file 字段相对 sidecar 数据根(形如 attachments/{task_id}/{name})
                let report = self
                    .report_path
                    .clone()
                    .unwrap_or_else(|| Self::DEFAULT_REPORT_PATH.into());
                std::path::Path::new(&report)
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| ".".into())
            },
            deadline_days: self.deadline_days.unwrap_or(3),
            interval_secs: self.interval_secs.unwrap_or(Self::DEFAULT_INTERVAL_SECS),
            todo_sync: self.todo_sync.unwrap_or(true),
            vault_sync: self.vault_sync.unwrap_or(true),
            vault_course_dir: self
                .vault_course_dir
                .clone()
                .unwrap_or_else(|| "01-Courses-课程/2026-Fall-大一上".into()),
            vault_courses,
            vault_task_dirs: ru41::ports::vault::VaultTaskDirs::parse(
                self.vault_task_dirs.as_deref(),
            )?,
            notify_target,
        })
    }
}
