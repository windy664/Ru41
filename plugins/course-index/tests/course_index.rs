use ru41_plugin_course_index::{
    CourseEntry, CourseIndexConfig, WorkItem, render_course_index, render_overview,
};
use std::collections::{BTreeMap, BTreeSet};

fn config() -> CourseIndexConfig {
    let mut aliases = BTreeMap::new();
    aliases.insert(
        "思想道德与法治".to_owned(),
        "思想道德修养与法律基础".to_owned(),
    );
    aliases.insert("大学英语新视野".to_owned(), "大学英语新视野".to_owned());
    CourseIndexConfig {
        vault_dir: "01-Courses-课程/2026-Fall-大一上".to_owned(),
        aliases,
        interval_secs: 3600,
    }
}

fn entry(vault_name: &str, sources: &[&str]) -> CourseEntry {
    CourseEntry {
        vault_name: vault_name.to_owned(),
        source_names: sources.iter().map(|s| (*s).to_owned()).collect(),
        class_group: Some("(2026-2027-1)-GE1050-02".to_owned()),
        teachers: BTreeSet::from(["曾尧冰".to_owned()]),
        rooms: BTreeSet::from(["B101".to_owned(), "C301".to_owned()]),
        slots: BTreeSet::from(["1-2节".to_owned(), "13-14节".to_owned()]),
        weeks: BTreeSet::from(["5-18周".to_owned()]),
        works: vec![WorkItem {
            platform: "智慧树".to_owned(),
            title: "随堂作业".to_owned(),
            deadline: Some("2026-11-08 00:00:00".to_owned()),
            status: Some("待批阅".to_owned()),
        }],
    }
}

#[test]
fn canonical_name_uses_alias_first() {
    let config = config();
    assert_eq!(config.canonical("思想道德与法治"), "思想道德修养与法律基础");
    // 没有别名时保留源名。
    assert_eq!(config.canonical("办公软件"), "办公软件");
}

#[test]
fn course_index_contains_schedule_works_and_notes() {
    let entry = entry(
        "思想道德修养与法律基础",
        &["思想道德与法治", "思想道德修养与法律基础"],
    );
    let text = render_course_index(&entry, &["课堂笔记.md".to_owned()]);
    assert!(text.contains("# 思想道德修养与法律基础 课程索引"), "{text}");
    assert!(text.contains("GE1050-02"), "{text}");
    assert!(text.contains("曾尧冰"), "{text}");
    assert!(text.contains("1-2节"), "{text}");
    assert!(text.contains("随堂作业"), "{text}");
    assert!(text.contains("课堂笔记.md"), "{text}");
    // 各平台原名都要记下来，方便人工核对是不是同一门课。
    assert!(text.contains("各平台名称"), "{text}");
}

#[test]
fn overview_lists_every_course() {
    let entries = vec![
        entry("高等数学I(理)", &["高等数学I(理)"]),
        entry("思想道德修养与法律基础", &["思想道德与法治"]),
    ];
    let text = render_overview(&entries);
    assert!(text.contains("| 课程目录 |"), "{text}");
    assert!(text.contains("高等数学I(理)"), "{text}");
    assert!(text.contains("思想道德与法治"), "{text}");
}

#[test]
fn course_without_schedule_still_gets_index() {
    // 只有平台作业、没有课表记录的课程也要有索引（别让 vault 少目录）。
    let mut entry = entry("大学英语新视野", &["大学英语新视野"]);
    entry.class_group = None;
    entry.slots.clear();
    entry.weeks.clear();
    entry.teachers.clear();
    entry.rooms.clear();
    let text = render_course_index(&entry, &[]);
    assert!(text.contains("课表里没有这门课的记录"), "{text}");
    assert!(text.contains("暂无笔记"), "{text}");
}
