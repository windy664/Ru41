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
"思想道德与法治" = "思想道德修养与法律基础"
"#,
    )
    .expect("config should parse");
    let section = config
        .sources
        .get::<ru41_plugin_jwxt::JwxtSection>("jwxt")
        .expect("jwxt section");
    assert_eq!(
        section.course_aliases.as_ref().and_then(|m| m.get("思想道德与法治")).map(String::as_str),
        Some("思想道德修养与法律基础")
    );
}
