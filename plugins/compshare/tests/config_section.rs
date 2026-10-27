//! [sources.compshare] 解析（原 tests/config.rs，随模块迁入）。

use ru41::bootstrap::config::Config;

#[test]
fn parses_compshare_sources_section_and_rejects_unknown_fields() {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.compshare]\nproject_id = \"org-test\"\nimage_id = \"image-1\"\ninterval_secs = 60\ncookie = \"U_USER_EMAIL=user%40example.edu.cn; U_CSRF_TOKEN=csrf\"\nnotify = true\n",
    )
    .unwrap();
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    let section = config
        .sources
        .get_or_default::<ru41_plugin_compshare::CompshareSection>("compshare");
    assert_eq!(section.project_id.as_deref(), Some("org-test"));
    assert_eq!(section.image_id.as_deref(), Some("image-1"));
    assert_eq!(section.interval_secs, Some(60));
    assert!(section.is_ready());
    let resolved = section.resolved("user:U1".into()).unwrap();
    assert_eq!(resolved.gpu_type, "P40");
    assert_eq!(resolved.region, "cn-wlcb");
    assert_eq!(resolved.zone, "cn-wlcb-01");
    assert_eq!(resolved.disk_size_gb, 100);
    assert_eq!(resolved.notify_target, "user:U1");

    std::fs::write(
        cwd.path().join("ru41.toml"),
        "[sources.compshare]\nproject_id = \"org-test\"\nunknown = 1\n",
    )
    .unwrap();
    // 分布式配置：[sources] 层不再逐字段校验，未知字段在 Section 读取时报错。
    let (config, _) = Config::load_from(None, cwd.path(), None).unwrap();
    assert!(
        config
            .sources
            .get::<ru41_plugin_compshare::CompshareSection>("compshare")
            .is_err()
    );
}
