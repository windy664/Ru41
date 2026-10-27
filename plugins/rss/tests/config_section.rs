//! [sources.rss] 解析（原 tests/config.rs，随模块迁入）。

use ru41::bootstrap::config::Config;

#[test]
fn parses_rss_sources_section() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.rss]\nfeeds = [\"https://example.com/feed.xml\"]\nkeywords = [\"agent\"]\nmax_items_per_feed = 20\nnotify = false\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    let rss = config
        .sources
        .get_or_default::<ru41_plugin_rss::RssSection>("rss");
    assert_eq!(
        rss.feeds.as_deref(),
        Some(&["https://example.com/feed.xml".to_owned()][..])
    );
    assert_eq!(rss.keywords.as_deref(), Some(&["agent".to_owned()][..]));
    assert_eq!(rss.max_items_per_feed, Some(20));
    assert_eq!(rss.notify, Some(false));
    assert_eq!(rss.interval_secs, None);
}
