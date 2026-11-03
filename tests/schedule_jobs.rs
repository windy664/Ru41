use ru41::bootstrap::assembly::scheduled_jobs;
use ru41::bootstrap::config::Config;

fn load(toml: &str) -> Config {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("ru41.toml"), toml).unwrap();
    Config::load_from(None, cwd.path(), None).unwrap().0
}

/// 全部附属插件化后内建注册表为空：schedule list 优雅返回空列表、不 panic。
/// 插件侧的 job 注册由各自 DEF（如 planner 的 todo-remind）在加载 .so 后提供，
/// 见 plugins/planner 的测试与端到端验证。
#[test]
fn empty_builtin_registry_is_graceful() {
    let jobs = scheduled_jobs(&load(""));
    assert!(
        jobs.is_empty(),
        "内建模块应已全部插件化，空配置下不应有任何内建 job"
    );
}
