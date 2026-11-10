//! [sources.arxiv] 解析（原 tests/config.rs，随模块迁入）。

use ru41::bootstrap::config::Config;

#[test]
fn parses_arxiv_sources_section() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.arxiv]\ncategories = [\"cs.CR\", \"cs.AI\"]\nkeywords = [\"prompt injection\"]\ninterests = \"AI 安全\"\ninterval_secs = 3600\nmax_recommendations = 2\nnotify = true\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    let arxiv = config
        .sources
        .get_or_default::<ru41_plugin_papers::ArxivSection>("arxiv");
    assert_eq!(
        arxiv.categories.as_deref(),
        Some(&["cs.CR".to_owned(), "cs.AI".to_owned()][..])
    );
    assert_eq!(
        arxiv.keywords.as_deref(),
        Some(&["prompt injection".to_owned()][..])
    );
    assert_eq!(arxiv.interests.as_deref(), Some("AI 安全"));
    assert_eq!(arxiv.interval_secs, Some(3600));
    assert_eq!(arxiv.notify, Some(true));
    assert_eq!(arxiv.max_results, None);
    assert_eq!(arxiv.max_recommendations, Some(2));

    let resolved = arxiv.resolved(None).unwrap();
    assert_eq!(resolved.interval_secs, 3600);
    assert_eq!(resolved.max_recommendations, 2);
    assert_eq!(
        ru41_plugin_papers::ArxivSection::DEFAULT_INTERVAL_SECS,
        86_400
    );
    let defaults = ru41_plugin_papers::PapersConfig::default();
    assert_eq!(defaults.interval_secs, 86_400);
    assert_eq!(defaults.max_recommendations, 3);
}
