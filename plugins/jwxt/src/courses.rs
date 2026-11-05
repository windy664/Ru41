//! 课程目录同步：以正方教务的课表快照为准，在 Vault 里为每门课建目录并写课程索引。
//!
//! 课表是课程清单的权威来源（教务系统导出，完整且准），所以课程目录直接由
//! 课表生成；智慧树/学习通等平台的作业文件由各自插件写在本课程目录下，
//! 索引只列出目录里已有的文件，不复制它们的内容。
//!
//! 目录名默认用课表里的课程名。Vault 里已有同名课程但叫法不同（课表
//! 「思想道德与法治」/ 已有目录「思想道德修养与法律基础」）时，用配置里的
//! 别名表归一，避免动用户已经写好的笔记。

use anyhow::Result;
use ru41::domain::timetable::{StoredMeeting, TIMETABLE_DOC_KIND};
use ru41::ports::{storage::TaskStore, vault::Vault};
use serde::Serialize;
use std::{collections::BTreeMap, collections::BTreeSet, sync::Arc};

/// 一门课（按课表课程号聚合）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CourseEntry {
    /// Vault 课程目录名。
    pub vault_name: String,
    /// 课表里的课程名。
    pub course: String,
    /// 课程号（class_group）。
    pub class_group: Option<String>,
    pub teachers: BTreeSet<String>,
    pub rooms: BTreeSet<String>,
    pub slots: BTreeSet<String>,
    pub weekdays: BTreeSet<u8>,
    pub weeks: BTreeSet<String>,
}

impl CourseEntry {
    /// 上课时间摘要：周一 1-2节、周三 11-12节 …
    pub fn schedule_summary(&self) -> String {
        self.weekdays
            .iter()
            .map(|day| {
                let slots = self.slots.iter().cloned().collect::<Vec<_>>().join("、");
                format!("周{} {slots}", weekday_name(*day))
            })
            .collect::<Vec<_>>()
            .join("；")
    }
}

fn weekday_name(weekday: u8) -> &'static str {
    match weekday {
        1 => "一",
        2 => "二",
        3 => "三",
        4 => "四",
        5 => "五",
        6 => "六",
        7 => "日",
        _ => "?",
    }
}

/// 运行时配置。
#[derive(Debug, Clone)]
pub struct CourseSyncConfig {
    /// Vault 内课程学期目录（相对 vault 根）。
    pub vault_dir: String,
    /// 课表课程名 → Vault 目录名。
    pub aliases: BTreeMap<String, String>,
}

impl CourseSyncConfig {
    /// 归一课程名：别名表优先，否则用课表原名。
    pub fn canonical(&self, course: &str) -> String {
        self.aliases
            .get(course)
            .cloned()
            .unwrap_or_else(|| course.to_owned())
    }
}

/// 一轮同步的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CourseSyncReport {
    /// 课表里的课程数。
    pub courses: usize,
    /// 写入索引的课程数。
    pub indexed: usize,
    /// 新建的课程目录。
    pub created_dirs: Vec<String>,
    /// 写失败的文件。
    pub failed: Vec<String>,
}

/// 课程目录同步器。
pub struct CourseSyncService {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    config: CourseSyncConfig,
}

impl CourseSyncService {
    pub fn new(store: Arc<dyn TaskStore>, vault: Arc<dyn Vault>, config: CourseSyncConfig) -> Self {
        Self {
            store,
            vault,
            config,
        }
    }

    /// 按课表快照聚合课程（同一课程号的行课合并成门）。
    pub async fn collect(&self) -> Result<Vec<CourseEntry>> {
        let docs = self.store.documents(TIMETABLE_DOC_KIND, 20)?;
        let mut entries: BTreeMap<String, CourseEntry> = BTreeMap::new();
        for doc in docs {
            let Some(meetings) = doc.value.get("meetings").and_then(|m| m.as_array()) else {
                continue;
            };
            for raw in meetings {
                let Ok(meeting) = serde_json::from_value::<StoredMeeting>(raw.clone()) else {
                    continue;
                };
                let name = self.config.canonical(&meeting.course);
                let entry = entries.entry(name.clone()).or_insert_with(|| CourseEntry {
                    vault_name: name,
                    course: meeting.course.clone(),
                    ..CourseEntry::default()
                });
                if entry.class_group.is_none() && !meeting.class_group.is_empty() {
                    entry.class_group = Some(meeting.class_group.clone());
                }
                if !meeting.teacher.is_empty() {
                    entry.teachers.insert(meeting.teacher.clone());
                }
                if !meeting.classroom.is_empty() {
                    entry.rooms.insert(meeting.classroom.clone());
                }
                if !meeting.sections_raw.is_empty() {
                    entry.slots.insert(meeting.sections_raw.clone());
                }
                if !meeting.weeks_raw.is_empty() {
                    entry.weeks.insert(meeting.weeks_raw.clone());
                }
                entry.weekdays.insert(meeting.weekday);
            }
        }
        Ok(entries.into_values().collect())
    }

    /// 一轮同步：每门课写课程索引，学期根写课程总览。
    pub async fn sync(&self) -> Result<CourseSyncReport> {
        let entries = self.collect().await?;
        let mut report = CourseSyncReport {
            courses: entries.len(),
            ..CourseSyncReport::default()
        };
        let base = self.config.vault_dir.trim_end_matches('/').to_owned();
        let existing = self.existing_dirs(&base).await?;
        for entry in &entries {
            let dir = format!("{base}/{}", entry.vault_name);
            if !existing.contains(&entry.vault_name) {
                report.created_dirs.push(entry.vault_name.clone());
            }
            let files = self.course_files(&dir).await?;
            let relative = format!("{dir}/课程索引.md");
            let body = render_course_index(entry, &files);
            match self.vault.write_file(&relative, &body) {
                Ok(()) => report.indexed += 1,
                Err(error) => report.failed.push(format!("{relative}: {error:#}")),
            }
        }
        let overview = format!("{base}/课程总览.md");
        match self.vault.write_file(&overview, &render_overview(&entries)) {
            Ok(()) => {}
            Err(error) => report.failed.push(format!("{overview}: {error:#}")),
        }
        Ok(report)
    }

    /// 学期目录下已有的课程目录名。
    async fn existing_dirs(&self, base: &str) -> Result<BTreeSet<String>> {
        let head = self.vault.remote_head()?;
        let files = self.vault.list_files(&head, &format!("{base}/"))?;
        Ok(files
            .into_iter()
            .filter_map(|file| {
                file.strip_prefix(&format!("{base}/"))
                    .and_then(|rest| rest.split_once('/'))
                    .map(|(dir, _)| dir.to_owned())
            })
            .collect())
    }

    /// 课程目录下已有的文件（笔记、平台作业文件等）。
    async fn course_files(&self, dir: &str) -> Result<Vec<String>> {
        let head = self.vault.remote_head()?;
        let files = self.vault.list_files(&head, &format!("{dir}/"))?;
        let mut names: Vec<String> = files
            .into_iter()
            .filter_map(|file| file.strip_prefix(&format!("{dir}/")).map(str::to_owned))
            .filter(|name| !name.is_empty() && !name.contains('/'))
            .filter(|name| name != "课程索引.md")
            .collect();
        names.sort();
        Ok(names)
    }
}

/// 单门课的索引正文。
pub fn render_course_index(entry: &CourseEntry, files: &[String]) -> String {
    let mut lines = vec![format!("# {} 课程索引", entry.vault_name), String::new()];
    lines.push("> 由 jwxt（教务课表）自动生成；手动改动会被下次同步覆盖。".to_owned());
    lines.push(String::new());
    lines.push("## 课表".to_owned());
    if let Some(group) = &entry.class_group {
        lines.push(format!("- 课程号：{group}"));
    }
    lines.push(format!("- 上课时间：{}", entry.schedule_summary()));
    if !entry.teachers.is_empty() {
        lines.push(format!(
            "- 教师：{}",
            entry
                .teachers
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("、")
        ));
    }
    if !entry.weeks.is_empty() {
        lines.push(format!(
            "- 周次：{}",
            entry.weeks.iter().cloned().collect::<Vec<_>>().join("、")
        ));
    }
    if !entry.rooms.is_empty() {
        lines.push(format!(
            "- 教室：{}",
            entry.rooms.iter().cloned().collect::<Vec<_>>().join("、")
        ));
    }
    lines.push(String::new());
    lines.push("## 本目录文件".to_owned());
    if files.is_empty() {
        lines.push("- 暂无（课堂笔记、平台作业等会出现在这里）".to_owned());
    } else {
        for file in files {
            lines.push(format!("- [{file}]({file})"));
        }
    }
    lines.join("\n")
}

/// 学期根的课程总览。
pub fn render_overview(entries: &[CourseEntry]) -> String {
    let mut lines = vec!["# 课程总览".to_owned(), String::new()];
    lines.push("> 由 jwxt（教务课表）自动生成：课表里的课程就是全部课程。".to_owned());
    lines.push(String::new());
    lines.push("| 课程目录 | 课表课程 | 课程号 | 教师 | 上课时间 | 周次 |".to_owned());
    lines.push("| --- | --- | --- | --- | --- | --- |".to_owned());
    for entry in entries {
        let group = entry.class_group.clone().unwrap_or_else(|| "—".to_owned());
        let teacher = entry
            .teachers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        let weeks = entry.weeks.iter().cloned().collect::<Vec<_>>().join("、");
        lines.push(format!(
            "| [{}]({}/课程索引.md) | {} | {group} | {teacher} | {} | {weeks} |",
            entry.vault_name,
            entry.vault_name,
            entry.course,
            entry.schedule_summary()
        ));
    }
    lines.join("\n")
}

/// 轮询任务接口：课程目录同步每小时跑一次。
impl ru41::ingress::schedule::PollJob for CourseSyncService {
    fn job_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(3_600)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move { Ok(serde_json::to_value(self.sync().await?)?) })
    }
}
