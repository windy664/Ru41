//! 课表数据契约：jwxt 写入快照文档，planner 等消费方按周次读取渲染——
//! 这些类型与纯函数是跨附属的共享契约，住在 domain，两侧各自 import。
//! 存储契约：documents("jwxt_timetable", key = "{xnm}#{xqm}")，value = TimetableSnapshot。

use crate::ports::storage::TaskStore;
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate, NaiveTime};
use std::sync::Arc;

/// 课表快照的文档 kind（jwxt 写、其他模块读；改动需同步两侧）。
pub const TIMETABLE_DOC_KIND: &str = "jwxt_timetable";

/// 课表快照记录（jwxt_timetable 的 value）。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TimetableSnapshot {
    pub xnm: String,
    pub xqm: String,
    pub hash: u64,
    pub updated_at: i64,
    pub meetings: Vec<StoredMeeting>,
}

/// 快照里的行课条目（CourseMeeting 的 JSON 形态，QQ 命令也读这一份）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoredMeeting {
    pub course: String,
    pub course_id: String,
    pub teacher: String,
    pub classroom: String,
    pub weekday: u8,
    pub sections_raw: String,
    pub start_section: Option<u8>,
    pub end_section: Option<u8>,
    pub weeks_raw: String,
    pub weeks: Vec<u32>,
    pub class_type: String,
    pub class_group: String,
}

impl From<&CourseMeeting> for StoredMeeting {
    fn from(m: &CourseMeeting) -> Self {
        Self {
            course: m.course.clone(),
            course_id: m.course_id.clone(),
            teacher: m.teacher.clone(),
            classroom: m.classroom.clone(),
            weekday: m.weekday,
            sections_raw: m.sections_raw.clone(),
            start_section: m.start_section,
            end_section: m.end_section,
            weeks_raw: m.weeks_raw.clone(),
            weeks: m.weeks.clone(),
            class_type: m.class_type.clone(),
            class_group: m.class_group.clone(),
        }
    }
}

impl StoredMeeting {
    /// 提醒/回复里的一行：`- **10:40-12:00** 高等数学 @U102（张晓婷）`。
    /// 未配置作息表时回退到节次文本（`3-4节`）。
    pub fn display_line(&self, clock: Option<&SectionClock>) -> String {
        let time = clock
            .and_then(|c| c.range_label(self.start_section, self.end_section))
            .unwrap_or_else(|| match (self.start_section, self.end_section) {
                (Some(s), Some(e)) if s == e => format!("{s}节"),
                (Some(s), Some(e)) => format!("{s}-{e}节"),
                _ => self.sections_raw.clone(),
            });
        let teacher = if self.teacher.is_empty() {
            String::new()
        } else {
            format!("（{}）", self.teacher)
        };
        let room = if self.classroom.is_empty() {
            "待定".to_owned()
        } else {
            self.classroom.clone()
        };
        format!("- **{time}** {} @{room}{teacher}", self.course)
    }
}

/// 课表行课条目（kbList 的业务子集）。
#[derive(Debug, Clone, PartialEq)]
pub struct CourseMeeting {
    pub course: String,
    pub course_id: String,
    pub teacher: String,
    pub classroom: String,
    /// 星期 1-7。
    pub weekday: u8,
    /// 原始节次文本，如 "9-10节"。
    pub sections_raw: String,
    pub start_section: Option<u8>,
    pub end_section: Option<u8>,
    /// 解析后的周次列表（zcd）。
    pub weeks: Vec<u32>,
    pub weeks_raw: String,
    pub class_type: String,
    pub class_group: String,
    pub xnm: String,
    pub xqm: String,
}

impl CourseMeeting {
    /// 稳定签名：变更检测的粒度（同一门课换教室/换周次即视为变更）。
    pub fn signature(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.course_id,
            self.weekday,
            self.sections_raw,
            self.weeks_raw,
            self.classroom,
            self.teacher
        )
    }
}

pub const WEEKDAY_NAMES: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

/// 按日期推断当前学期：9-12 月 → (当年, 3)；1 月 → (去年, 3)；2-8 月 → (去年, 12)。
/// 短学期（xqm=16）不参与自动推断，需要时配置里显式指定。
pub fn infer_term(today: NaiveDate) -> (String, String) {
    let (year, month) = (today.year(), today.month());
    match month {
        9..=12 => (year.to_string(), "3".to_owned()),
        1 => ((year - 1).to_string(), "3".to_owned()),
        _ => ((year - 1).to_string(), "12".to_owned()),
    }
}

/// 配置优先的学期解析。
pub fn resolve_term(xnm: Option<&str>, xqm: Option<&str>, today: NaiveDate) -> (String, String) {
    let (inferred_xnm, inferred_xqm) = infer_term(today);
    (
        xnm.unwrap_or(&inferred_xnm).to_owned(),
        xqm.unwrap_or(&inferred_xqm).to_owned(),
    )
}

/// 学期第几周（semester_start 为第一周周一）；开学前返回 0。
pub fn week_of(semester_start: NaiveDate, date: NaiveDate) -> i64 {
    (date - semester_start).num_days().div_euclid(7) + 1
}

/// 日期 → 正方星期号（周一=1 … 周日=7）。
fn weekday_no(date: NaiveDate) -> u8 {
    (date.weekday().number_from_monday()) as u8
}

pub fn weekday_name(date: NaiveDate) -> &'static str {
    WEEKDAY_NAMES[(date.weekday().number_from_monday() - 1) as usize]
}

/// 取某天要上的课（星期匹配 + 周次在 zcd 范围内）。
pub fn meetings_on(
    meetings: &[CourseMeeting],
    semester_start: NaiveDate,
    date: NaiveDate,
) -> Vec<CourseMeeting> {
    let week = week_of(semester_start, date);
    let weekday = weekday_no(date);
    meetings
        .iter()
        .filter(|m| m.weekday == weekday && m.weeks.contains(&(week as u32)))
        .cloned()
        .collect()
}

/// 节次时刻表：把 "3-4节" 换算成 "10:40-12:00"。教务接口不暴露作息时间，
/// 由配置 `section_times` + `class_duration_mins` 提供。
#[derive(Debug, Clone)]
pub struct SectionClock {
    /// 索引 = 节次 - 1。
    pub times: Vec<NaiveTime>,
    /// 单节时长（分钟）。
    pub duration_mins: u64,
}

impl SectionClock {
    /// 节次范围 → "HH:MM-HH:MM"；超出时刻表范围返回 None（调用方回退到节次文本）。
    pub fn range_label(&self, start: Option<u8>, end: Option<u8>) -> Option<String> {
        let (s, e) = (start?, end?);
        let start_t = *self.times.get(s as usize - 1)?;
        let end_t = *self.times.get(e as usize - 1)?;
        let end_t = end_t + Duration::minutes(self.duration_mins as i64);
        Some(format!(
            "{}-{}",
            start_t.format("%H:%M"),
            end_t.format("%H:%M")
        ))
    }
}

/// 快照文档 → CourseMeeting 列表（快照的 JSON 形态还原成业务类型）。
pub fn snapshot_meetings(snapshot: &TimetableSnapshot) -> Vec<CourseMeeting> {
    snapshot
        .meetings
        .iter()
        .map(|m| CourseMeeting {
            course: m.course.clone(),
            course_id: m.course_id.clone(),
            teacher: m.teacher.clone(),
            classroom: m.classroom.clone(),
            weekday: m.weekday,
            sections_raw: m.sections_raw.clone(),
            start_section: m.start_section,
            end_section: m.end_section,
            weeks: m.weeks.clone(),
            weeks_raw: m.weeks_raw.clone(),
            class_type: m.class_type.clone(),
            class_group: m.class_group.clone(),
            xnm: snapshot.xnm.clone(),
            xqm: snapshot.xqm.clone(),
        })
        .collect()
}

/// 读当前学期的课表快照；没有缓存返回 None（调用方给「暂无课表」文案）。
pub fn timetable_snapshot(store: &Arc<dyn TaskStore>) -> Result<Option<TimetableSnapshot>> {
    let (xnm, xqm) = resolve_term(None, None, chrono::Local::now().date_naive());
    let key = format!("{xnm}#{xqm}");
    let Some(doc) = store.document(TIMETABLE_DOC_KIND, &key)? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(doc.value)?))
}

/// 从缓存快照构造某天的课表回复（QQ 命令用；不触网）。
pub fn describe_day_from_store(
    store: &Arc<dyn TaskStore>,
    semester_start: Option<NaiveDate>,
    clock: Option<&SectionClock>,
    date: NaiveDate,
) -> Result<String> {
    let Some(snapshot) = timetable_snapshot(store)? else {
        return Ok(
            "## 暂无课表缓存\n\n课表还没同步成功：`jwxt run` 未运行，或学校教务系统当前不可用（恢复后会自动同步）。"
                .to_owned(),
        );
    };
    let Some(start) = semester_start else {
        return Ok(
            "## 缺少学期配置\n\n请在配置里设置 `sources.jwxt.semester_start`（学期第一周周一），才能按周次过滤课表。"
                .to_owned(),
        );
    };
    let meetings = snapshot_meetings(&snapshot);
    let day_meetings = meetings_on(&meetings, start, date);
    let title = format!(
        "## 📅 {} {} · 第{}周\n\n",
        date.format("%m月%d日"),
        weekday_name(date),
        week_of(start, date)
    );
    let body = if day_meetings.is_empty() {
        "没有课 🎉".to_owned()
    } else {
        let mut lines: Vec<String> = day_meetings
            .iter()
            .map(|m| StoredMeeting::from(m).display_line(clock))
            .collect();
        lines.sort();
        format!("{}\n\n共 {} 节课", lines.join("\n"), lines.len())
    };
    Ok(format!("{title}{body}"))
}

/// 多日课表紧凑摘要（/日程 合并视图用）：每天一行「MM-DD 周X：课(第N节)@教室」。
/// 没配置学期起点、没有课表缓存或当天无课的日子都不产生行；出错时返回空列表不阻塞日程视图。
pub fn courses_summary_from_store(
    store: &Arc<dyn TaskStore>,
    semester_start: Option<NaiveDate>,
    start_date: NaiveDate,
    days: u32,
) -> Vec<String> {
    let Some(start) = semester_start else {
        return Vec::new();
    };
    let summary = (|| -> Result<Vec<String>> {
        let Some(snapshot) = timetable_snapshot(store)? else {
            return Ok(Vec::new());
        };
        let meetings = snapshot_meetings(&snapshot);
        let mut out = Vec::new();
        for offset in 0..days {
            let date = start_date + Duration::days(offset as i64);
            let day = meetings_on(&meetings, start, date);
            if day.is_empty() {
                continue;
            }
            let items: Vec<String> = day
                .iter()
                .map(|m| {
                    let section = m
                        .start_section
                        .map(|s| format!("（第{s}节）"))
                        .unwrap_or_default();
                    let loc = if m.classroom.is_empty() {
                        String::new()
                    } else {
                        format!("@{}", m.classroom)
                    };
                    format!("{}{}{}", m.course, section, loc)
                })
                .collect();
            out.push(format!(
                "- {} {}：{}",
                date.format("%m-%d"),
                weekday_name(date),
                items.join("、")
            ));
        }
        Ok(out)
    })();
    summary.unwrap_or_default()
}
