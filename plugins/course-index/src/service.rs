//! 课程索引：把课表（jwxt 快照）、智慧树、学习通三边的课程交叉枚举，
//! 在 Vault 里为每门课建目录并写课程索引，另在学期根写一份课程总览。
//!
//! 同一门课在三边的名字常常不同（课表「思想道德与法治」、平台「思想道德修养与
//! 法律基础」），归一靠配置里的别名表；没有别名时按源名建目录。
//!
//! 数据来源都是共享存储里的记录，本插件只读不写别人的记录：
//! - `jwxt_timetable`：课表快照（课程号、节次、周次、教室、教师）
//! - `chaoxing_work` / `chaoxing_meta`：智慧树课程与作业条目
//! - `teachermate_work`：学习通（微助教）作业条目

use anyhow::Result;
use ru41::ports::{storage::TaskStore, vault::Vault};
use serde::Serialize;
use std::{collections::BTreeMap, collections::BTreeSet, sync::Arc, time::Duration};

/// 单个平台的作业条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkItem {
    pub platform: String,
    pub title: String,
    pub deadline: Option<String>,
    pub status: Option<String>,
}

/// 一门课的合并视图（课表信息 + 各平台作业 + 出现过的源名字）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CourseEntry {
    /// Vault 里的课程目录名（归一后）。
    pub vault_name: String,
    /// 归一前出现过的课程名（各平台可能不同）。
    pub source_names: Vec<String>,
    /// 课表课程号（class_group），有课表才有。
    pub class_group: Option<String>,
    pub teachers: BTreeSet<String>,
    pub rooms: BTreeSet<String>,
    pub slots: BTreeSet<String>,
    pub weeks: BTreeSet<String>,
    pub works: Vec<WorkItem>,
}

/// 运行时配置。
#[derive(Debug, Clone)]
pub struct CourseIndexConfig {
    /// Vault 内课程学期目录（相对 vault 根）。
    pub vault_dir: String,
    /// 源课程名 → 目录名。
    pub aliases: BTreeMap<String, String>,
    pub interval_secs: u64,
}

impl CourseIndexConfig {
    /// 归一课程名：优先别名表，其次去掉尾部的「 I」这类罗马数字后缀。
    pub fn canonical(&self, source_name: &str) -> String {
        if let Some(alias) = self.aliases.get(source_name) {
            return alias.clone();
        }
        source_name.to_owned()
    }
}

/// 一轮同步的结果（给 CLI/日志看）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CourseIndexReport {
    /// 归一后的课程数。
    pub courses: usize,
    /// 写入索引文件的课程数。
    pub indexed: usize,
    /// 新建的课程目录数。
    pub created_dirs: Vec<String>,
    /// 平台作业条目总数。
    pub works: usize,
    /// 写失败的文件（不阻断其他课程）。
    pub failed: Vec<String>,
}

/// 课程索引同步器。
pub struct CourseIndexTracker {
    store: Arc<dyn TaskStore>,
    vault: Arc<dyn Vault>,
    config: CourseIndexConfig,
}

impl CourseIndexTracker {
    pub fn new(
        store: Arc<dyn TaskStore>,
        vault: Arc<dyn Vault>,
        config: CourseIndexConfig,
    ) -> Self {
        Self {
            store,
            vault,
            config,
        }
    }

    /// 交叉枚举三边课程，合并成按目录名归一后的课程表。
    pub async fn collect(&self) -> Result<Vec<CourseEntry>> {
        let mut entries: BTreeMap<String, CourseEntry> = BTreeMap::new();
        for meeting in self.timetable_meetings().await? {
            let source = meeting.course.clone();
            let name = self.config.canonical(&source);
            let entry = entries.entry(name.clone()).or_insert_with(|| CourseEntry {
                vault_name: name,
                ..CourseEntry::default()
            });
            if !entry.source_names.contains(&source) {
                entry.source_names.push(source);
            }
            if entry.class_group.is_none() {
                entry.class_group = meeting.class_group.clone();
            }
            if let Some(teacher) = meeting.teacher.filter(|t| !t.is_empty()) {
                entry.teachers.insert(teacher);
            }
            if let Some(room) = meeting.classroom.filter(|r| !r.is_empty()) {
                entry.rooms.insert(room);
            }
            if let Some(slots) = meeting.sections_raw.filter(|s| !s.is_empty()) {
                entry.slots.insert(slots);
            }
            if let Some(weeks) = meeting.weeks_raw.filter(|w| !w.is_empty()) {
                entry.weeks.insert(weeks);
            }
        }
        for (platform, kind) in [("智慧树", "chaoxing_work"), ("学习通", "teachermate_work")]
        {
            for work in self.platform_works(kind).await? {
                let name = self.config.canonical(&work.course);
                let entry = entries.entry(name.clone()).or_insert_with(|| CourseEntry {
                    vault_name: name,
                    ..CourseEntry::default()
                });
                if !entry.source_names.contains(&work.course) {
                    entry.source_names.push(work.course);
                }
                entry.works.push(WorkItem {
                    platform: platform.to_owned(),
                    title: work.title,
                    deadline: work.deadline,
                    status: work.status,
                });
            }
        }
        for titles in self.chaoxing_titles().await? {
            let name = self.config.canonical(&titles);
            let entry = entries.entry(name.clone()).or_insert_with(|| CourseEntry {
                vault_name: name,
                ..CourseEntry::default()
            });
            if !entry.source_names.contains(&titles) {
                entry.source_names.push(titles);
            }
        }
        Ok(entries.into_values().collect())
    }

    /// 一轮同步：写每门课的索引 + 学期根的课程总览。
    pub async fn sync(&self) -> Result<CourseIndexReport> {
        let entries = self.collect().await?;
        let mut report = CourseIndexReport {
            courses: entries.len(),
            ..CourseIndexReport::default()
        };
        let base = self.config.vault_dir.trim_end_matches('/').to_owned();
        let existing = self.existing_course_dirs(&base).await?;
        for entry in &entries {
            let dir = format!("{base}/{}", entry.vault_name);
            let relative = format!("{dir}/课程索引.md");
            if !existing.contains(&entry.vault_name) {
                report.created_dirs.push(entry.vault_name.clone());
            }
            let notes = self.notes_in(&dir).await?;
            let body = render_course_index(entry, &notes);
            match self.vault.write_file(&relative, &body) {
                Ok(()) => report.indexed += 1,
                Err(error) => report.failed.push(format!("{relative}: {error:#}")),
            }
        }
        report.works = entries.iter().map(|entry| entry.works.len()).sum();
        let overview = format!("{base}/课程总览.md");
        match self.vault.write_file(&overview, &render_overview(&entries)) {
            Ok(()) => {}
            Err(error) => report.failed.push(format!("{overview}: {error:#}")),
        }
        Ok(report)
    }

    /// 学期目录下已有的课程目录名。
    async fn existing_course_dirs(&self, base: &str) -> Result<BTreeSet<String>> {
        let head = self.vault.remote_head()?;
        let files = self.vault.list_files(&head, &format!("{base}/"))?;
        let mut dirs = BTreeSet::new();
        for file in files {
            let rest = file.strip_prefix(&format!("{base}/")).unwrap_or("");
            if let Some((dir, _)) = rest.split_once('/') {
                dirs.insert(dir.to_owned());
            }
        }
        Ok(dirs)
    }

    /// 课程目录里已有的笔记（不含本次写的索引）。
    async fn notes_in(&self, dir: &str) -> Result<Vec<String>> {
        let head = self.vault.remote_head()?;
        let files = self.vault.list_files(&head, &format!("{dir}/"))?;
        Ok(files
            .into_iter()
            .filter_map(|file| file.strip_prefix(&format!("{dir}/")).map(str::to_owned))
            .filter(|name| name.ends_with(".md") && name != "课程索引.md")
            .collect())
    }

    async fn timetable_meetings(&self) -> Result<Vec<TimetableMeeting>> {
        let docs = self.store.documents("jwxt_timetable", 20)?;
        let mut meetings = Vec::new();
        for doc in docs {
            let Some(raw) = doc.value.get("meetings").and_then(|m| m.as_array()) else {
                continue;
            };
            for item in raw {
                let Some(meeting) = TimetableMeeting::from_json(item) else {
                    continue;
                };
                meetings.push(meeting);
            }
        }
        Ok(meetings)
    }

    async fn platform_works(&self, kind: &str) -> Result<Vec<PlatformWork>> {
        let docs = self.store.documents(kind, 500)?;
        let mut works = Vec::new();
        for doc in docs {
            let course = doc
                .value
                .get("course")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_owned();
            if course.is_empty() {
                continue;
            }
            works.push(PlatformWork {
                course,
                title: doc
                    .value
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("（未命名）")
                    .to_owned(),
                deadline: doc
                    .value
                    .get("end_at")
                    .and_then(|d| d.as_str())
                    .map(str::to_owned),
                status: doc
                    .value
                    .get("status")
                    .and_then(|s| s.as_str())
                    .map(str::to_owned),
            });
        }
        Ok(works)
    }

    /// 智慧树课程列表（只记课程名，没有作业条目时也建目录）。
    async fn chaoxing_titles(&self) -> Result<Vec<String>> {
        let docs = self.store.documents("chaoxing_meta", 50)?;
        let mut titles = Vec::new();
        for doc in docs {
            let Some(list) = doc.value.get("titles").and_then(|t| t.as_array()) else {
                continue;
            };
            for item in list {
                if let Some(title) = item.as_str() {
                    titles.push(title.to_owned());
                }
            }
        }
        titles.sort();
        titles.dedup();
        Ok(titles)
    }
}

/// 课表里的一行课。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimetableMeeting {
    pub course: String,
    pub class_group: Option<String>,
    pub teacher: Option<String>,
    pub classroom: Option<String>,
    pub sections_raw: Option<String>,
    pub weeks_raw: Option<String>,
}

impl TimetableMeeting {
    fn from_json(value: &serde_json::Value) -> Option<Self> {
        let course = value.get("course")?.as_str()?.trim().to_owned();
        if course.is_empty() {
            return None;
        }
        Some(Self {
            course,
            class_group: value
                .get("class_group")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            teacher: value
                .get("teacher")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            classroom: value
                .get("classroom")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            sections_raw: value
                .get("sections_raw")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            weeks_raw: value
                .get("weeks_raw")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
        })
    }
}

/// 平台上的一条作业。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformWork {
    pub course: String,
    pub title: String,
    pub deadline: Option<String>,
    pub status: Option<String>,
}

/// 单门课的索引正文。
pub fn render_course_index(entry: &CourseEntry, notes: &[String]) -> String {
    let mut lines = vec![format!("# {} 课程索引", entry.vault_name), String::new()];
    lines.push(
        "> 由 course-index 自动生成：课表 + 智慧树 + 学习通 交叉枚举；手动改动会被下次同步覆盖。"
            .to_owned(),
    );
    if entry.source_names.len() > 1 {
        lines.push(String::new());
        lines.push(format!("> 各平台名称：{}", entry.source_names.join(" / ")));
    }
    lines.push(String::new());
    lines.push("## 课表".to_owned());
    if entry.class_group.is_none() && entry.slots.is_empty() {
        lines.push("- 课表里没有这门课的记录".to_owned());
    } else {
        if let Some(group) = &entry.class_group {
            lines.push(format!("- 课程号：{group}"));
        }
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
        if !entry.slots.is_empty() {
            lines.push(format!(
                "- 上课节次：{}",
                entry.slots.iter().cloned().collect::<Vec<_>>().join("、")
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
    }
    lines.push(String::new());
    lines.push("## 平台作业".to_owned());
    if entry.works.is_empty() {
        lines.push("- 暂无作业记录".to_owned());
    } else {
        let mut by_platform: BTreeMap<&str, Vec<&WorkItem>> = BTreeMap::new();
        for work in &entry.works {
            by_platform
                .entry(work.platform.as_str())
                .or_default()
                .push(work);
        }
        for (platform, items) in by_platform {
            lines.push(format!("### {platform}（{}）", items.len()));
            for work in items {
                let deadline = work.deadline.as_deref().unwrap_or("未设截止");
                let status = work.status.as_deref().unwrap_or("");
                let suffix = if status.is_empty() {
                    String::new()
                } else {
                    format!("（{status}）")
                };
                lines.push(format!("- {}｜截止 {deadline}{suffix}", work.title));
            }
        }
    }
    lines.push(String::new());
    lines.push("## 本目录笔记".to_owned());
    if notes.is_empty() {
        lines.push("- 暂无笔记".to_owned());
    } else {
        for note in notes {
            lines.push(format!("- [{note}]({note})"));
        }
    }
    lines.join("\n")
}

/// 学期根的课程总览。
pub fn render_overview(entries: &[CourseEntry]) -> String {
    let mut lines = vec!["# 课程总览".to_owned(), String::new()];
    lines.push("> 由 course-index 自动生成：课表 + 智慧树 + 学习通 交叉枚举。".to_owned());
    lines.push(String::new());
    lines.push("| 课程目录 | 课表课程 | 教师 | 上课 | 平台作业 |".to_owned());
    lines.push("| --- | --- | --- | --- | --- |".to_owned());
    for entry in entries {
        let source = entry.source_names.join(" / ");
        let teacher = entry
            .teachers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        let slots = entry.slots.iter().cloned().collect::<Vec<_>>().join("、");
        let platforms: BTreeSet<&str> = entry
            .works
            .iter()
            .map(|work| work.platform.as_str())
            .collect();
        let platform = if platforms.is_empty() {
            "—".to_owned()
        } else {
            format!(
                "{}（{} 条）",
                platforms.iter().cloned().collect::<Vec<_>>().join("/"),
                entry.works.len()
            )
        };
        lines.push(format!(
            "| [{}]({}/{}) | {source} | {teacher} | {slots} | {platform} |",
            entry.vault_name, entry.vault_name, "课程索引.md"
        ));
    }
    lines.join("\n")
}

/// 轮询任务接口。
impl ru41::ingress::schedule::PollJob for CourseIndexTracker {
    fn job_interval(&self) -> Duration {
        Duration::from_secs(self.config.interval_secs)
    }

    fn poll_json(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + '_>>
    {
        Box::pin(async move { Ok(serde_json::to_value(self.sync().await?)?) })
    }
}
