use ru41::bootstrap::assembly::scheduled_jobs;
use ru41::bootstrap::config::Config;

fn load(toml: &str) -> Config {
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(cwd.path().join("ru41.toml"), toml).unwrap();
    Config::load_from(None, cwd.path(), None).unwrap().0
}

fn job<'a>(
    jobs: &'a [ru41::ingress::schedule::ScheduledJob],
    name: &str,
) -> &'a ru41::ingress::schedule::ScheduledJob {
    jobs.iter()
        .find(|j| j.name == name)
        .unwrap_or_else(|| panic!("job {name} not registered"))
}

#[test]
fn registers_planner_jobs() {
    // 整体插件化后：外部插件（jwxt/papers/rss/...）经 .so 加载，不在本测试的
    // 注册表内；内建模块只剩 planner 的 todo-remind。
    let jobs = scheduled_jobs(&load(""));
    assert!(jobs.iter().any(|j| j.name == "todo-remind"), "todo-remind missing");
}

#[test]
fn reports_disabled_without_config() {
    let jobs = scheduled_jobs(&load(""));
    assert!(!job(&jobs, "todo-remind").enabled);
    assert!(job(&jobs, "todo-remind").notes.is_some());
}

#[test]
fn flips_enabled_when_configured() {
    let config = load(
        "[notify]\nchannel = \"qq\"\n[channel.qq]\ndefault_notify_target = \"user:U1\"\n",
    );
    let jobs = scheduled_jobs(&config);
    assert!(job(&jobs, "todo-remind").enabled);
}

#[test]
fn exposes_poll_and_run_commands() {
    let jobs = scheduled_jobs(&load(""));
    let remind = job(&jobs, "todo-remind");
    assert_eq!(remind.poll_command, "todo remind --once");
    assert_eq!(remind.run_command, "todo remind");
    assert_eq!(remind.module, "planner");
}
