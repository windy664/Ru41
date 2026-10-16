use ru41::bootstrap::config::Config;
use std::path::PathBuf;

#[test]
fn missing_files_fall_back_to_defaults() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let (config, path) = Config::load_from(None, cwd.path(), Some(home.path())).unwrap();
    assert!(path.is_none());
    assert!(config.store.kind.is_none());
    assert!(config.worker.timeout_secs.is_none());
}

#[test]
fn cwd_ru41_toml_beats_home_config() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/ru41")).unwrap();
    std::fs::write(
        home.path().join(".config/ru41/config.toml"),
        "[store]\nkind = \"postgres\"\n",
    )
    .unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[store]\nkind = \"sqlite\"\npath = \"data/state.db\"\n[worker]\ntimeout_secs = 60\nsanitize = false\n",
    )
    .unwrap();
    let (config, path) = Config::load_from(None, cwd.path(), Some(home.path())).unwrap();
    assert_eq!(
        path.as_deref(),
        Some(cwd.path().join("ru41.toml").as_path())
    );
    assert_eq!(config.store.kind.as_deref(), Some("sqlite"));
    assert_eq!(
        config.store.path.as_deref(),
        Some(PathBuf::from("data/state.db").as_path())
    );
    assert_eq!(config.worker.timeout_secs, Some(60));
    assert_eq!(config.worker.sanitize, Some(false));
}

#[test]
fn home_config_used_when_cwd_has_none() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".config/ru41")).unwrap();
    std::fs::write(
        home.path().join(".config/ru41/config.toml"),
        "[agent.codex]\nbin = \"/opt/codex\"\nmodel = \"gpt-5\"\n[channel.qq]\napp_id = \"123\"\nbind_secret = \"s3cret\"\n",
    )
    .unwrap();
    let (config, path) = Config::load_from(None, cwd.path(), Some(home.path())).unwrap();
    assert!(path.unwrap().starts_with(home.path()));
    assert_eq!(
        config.agent.codex.bin.as_deref(),
        Some(PathBuf::from("/opt/codex").as_path())
    );
    assert_eq!(config.agent.codex.model.as_deref(), Some("gpt-5"));
    assert_eq!(config.channel.qq.app_id.as_deref(), Some("123"));
    assert_eq!(config.channel.qq.bind_secret.as_deref(), Some("s3cret"));
}

#[test]
fn parses_arxiv_sources_section() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.arxiv]\ncategories = [\"cs.CR\", \"cs.AI\"]\nkeywords = [\"prompt injection\"]\ninterests = \"AI 安全\"\ninterval_secs = 3600\nnotify = true\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    let arxiv = &config.sources.arxiv;
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
}

#[test]
fn parses_notify_section() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[notify]\nchannel = \"qq\"\n[channel.qq]\ndefault_notify_target = \"user:U1\"\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    assert_eq!(config.notify.channel.as_deref(), Some("qq"));
    assert_eq!(
        config.channel.qq.default_notify_target.as_deref(),
        Some("user:U1")
    );
}

#[test]
fn parses_rss_sources_section() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.rss]\nfeeds = [\"https://example.com/feed.xml\"]\nkeywords = [\"agent\"]\nmax_items_per_feed = 20\nnotify = false\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    let rss = &config.sources.rss;
    assert_eq!(
        rss.feeds.as_deref(),
        Some(&["https://example.com/feed.xml".to_owned()][..])
    );
    assert_eq!(rss.keywords.as_deref(), Some(&["agent".to_owned()][..]));
    assert_eq!(rss.max_items_per_feed, Some(20));
    assert_eq!(rss.notify, Some(false));
    assert_eq!(rss.interval_secs, None);
}

#[test]
fn explicit_path_wins_and_missing_explicit_path_fails() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("ru41.toml"), "[store]\nkind = \"sqlite\"\n").unwrap();
    let explicit = tempfile::tempdir().unwrap();
    let file = explicit.path().join("custom.toml");
    std::fs::write(
        &file,
        "[store]\nkind = \"postgres\"\nurl = \"postgres://x\"\n",
    )
    .unwrap();
    let (config, path) = Config::load_from(Some(&file), cwd.path(), None).unwrap();
    assert_eq!(path.as_deref(), Some(file.as_path()));
    assert_eq!(config.store.kind.as_deref(), Some("postgres"));
    assert!(Config::load_from(Some(&explicit.path().join("nope.toml")), cwd.path(), None).is_err());
}

#[test]
fn unknown_fields_and_invalid_toml_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.toml");
    std::fs::write(&file, "[store]\nkind = \"sqlite\"\nunknown = 1\n").unwrap();
    assert!(Config::load_from(Some(&file), dir.path(), None).is_err());
    std::fs::write(&file, "not toml [").unwrap();
    assert!(Config::load_from(Some(&file), dir.path(), None).is_err());
}
