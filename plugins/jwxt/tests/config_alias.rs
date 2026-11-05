use ru41_plugin_jwxt::JwxtSection;

#[test]
fn course_aliases_are_parsed_from_nested_table() {
    // [sources.jwxt.course_aliases] 是 jwxt 段的子表，serde 要能吃下。
    let section: JwxtSection = toml::from_str(
        r#"
username = "u"
password = "p"
vault_course_dir = "01-Courses-课程/2026-Fall-大一上"

[course_aliases]
"思想道德与法治" = "思想道德修养与法律基础"
"#,
    )
    .expect("section should parse");
    let config = section.course_sync_config().expect("course config");
    assert_eq!(
        config.aliases.get("思想道德与法治").map(String::as_str),
        Some("思想道德修养与法律基础")
    );
    assert_eq!(config.vault_dir, "01-Courses-课程/2026-Fall-大一上");
}
