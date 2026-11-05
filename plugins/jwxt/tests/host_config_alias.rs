use ru41::bootstrap::config::Config;

#[test]
fn host_config_keeps_nested_alias_table() {
    // 宿主解析路径：Config → SourcesSection → [sources.jwxt.course_aliases]。
    let config: Config = toml::from_str(
        r#"
[sources.jwxt]
username = "u"
password = "p"

[sources.jwxt.course_aliases]
"程序设计基础" = "C语言程序设计"
"#,
    )
    .expect("config should parse");
    let section = config
        .sources
        .get::<ru41_plugin_jwxt::JwxtSection>("jwxt")
        .expect("jwxt section");
    assert_eq!(
        section
            .course_aliases
            .as_ref()
            .and_then(|m| m.get("程序设计基础"))
            .map(String::as_str),
        Some("C语言程序设计")
    );
}
