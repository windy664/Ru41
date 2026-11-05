use ru41_plugin_jwxt::courses::{
    CourseEntry, CourseSyncConfig, render_course_index, render_overview,
};
use std::collections::{BTreeMap, BTreeSet};

fn config() -> CourseSyncConfig {
    let mut aliases = BTreeMap::new();
    aliases.insert(
        "思想道德与法治".to_owned(),
        "思想道德修养与法律基础".to_owned(),
    );
    CourseSyncConfig {
        vault_dir: "01-Courses-课程/2026-Fall-大一上".to_owned(),
        aliases,
    }
}

fn entry(course: &str, vault_name: &str) -> CourseEntry {
    CourseEntry {
        vault_name: vault_name.to_owned(),
        course: course.to_owned(),
        class_group: Some("(2026-2027-1)-GE1050-02".to_owned()),
        teachers: BTreeSet::from(["曾尧冰".to_owned()]),
        rooms: BTreeSet::from(["B101".to_owned()]),
        slots: BTreeSet::from(["1-2节".to_owned(), "13-14节".to_owned()]),
        weekdays: BTreeSet::from([1_u8, 3_u8]),
        weeks: BTreeSet::from(["5-18周".to_owned()]),
    }
}

#[test]
fn alias_maps_timetable_name_to_existing_vault_dir() {
    let config = config();
    assert_eq!(config.canonical("思想道德与法治"), "思想道德修养与法律基础");
    // 没配别名的课按课表原名建目录。
    assert_eq!(config.canonical("办公软件"), "办公软件");
}

#[test]
fn course_index_lists_schedule_and_directory_files() {
    let entry = entry("思想道德与法治", "思想道德修养与法律基础");
    let text = render_course_index(
        &entry,
        &["课堂笔记.md".to_owned(), "智慧树作业.md".to_owned()],
    );
    assert!(text.contains("# 思想道德修养与法律基础 课程索引"), "{text}");
    assert!(text.contains("GE1050-02"), "{text}");
    assert!(text.contains("曾尧冰"), "{text}");
    assert!(text.contains("周"), "{text}");
    assert!(text.contains("课堂笔记.md"), "{text}");
    assert!(text.contains("智慧树作业.md"), "{text}");
}

#[test]
fn overview_lists_all_courses_with_course_number() {
    let entries = vec![
        entry("高等数学I(理)", "高等数学I(理)"),
        entry("思想道德与法治", "思想道德修养与法律基础"),
    ];
    let text = render_overview(&entries);
    assert!(text.contains("| 课程目录 |"), "{text}");
    assert!(text.contains("高等数学I(理)"), "{text}");
    assert!(text.contains("思想道德与法治"), "{text}");
    assert!(text.contains("GE1050-02"), "{text}");
}

#[test]
fn schedule_summary_mentions_weekday_and_slot() {
    let entry = entry("高等数学I(理)", "高等数学I(理)");
    let summary = entry.schedule_summary();
    assert!(summary.contains("周一"), "{summary}");
    assert!(summary.contains("周三"), "{summary}");
    assert!(summary.contains("1-2节"), "{summary}");
}
