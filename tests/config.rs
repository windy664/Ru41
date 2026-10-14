use ru41::app::config::Config;
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
        "[agent.codex]\nbin = \"/opt/codex\"\nmodel = \"gpt-5\"\n[channel.qq]\napp_id = \"123\"\n",
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
