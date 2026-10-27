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
fn registers_all_module_jobs() {
    // 整体插件化后：外部插件（papers/rss/competitions/...）经 .so 加载，
    // 不在本测试的注册表内；这里只断言内建模块。
    let jobs = scheduled_jobs(&load(""));
    for name in ["youtube", "chaoxing", "teachermate", "jwxt", "todo-remind"] {
        assert!(jobs.iter().any(|j| j.name == name), "{name} missing");
    }
}

#[test]
fn reports_disabled_without_config() {
    let jobs = scheduled_jobs(&load(""));
    assert!(!job(&jobs, "youtube").enabled);
    assert!(!job(&jobs, "jwxt").enabled);
    assert!(!job(&jobs, "todo-remind").enabled);
    assert!(job(&jobs, "youtube").notes.is_some());
}

#[test]
fn flips_enabled_when_configured() {
    let config = load(
        "[sources.youtube]\nchannels = [\"@test\"]\ninterval_secs = 3600\n\
         [notify]\nchannel = \"qq\"\n[channel.qq]\ndefault_notify_target = \"user:U1\"\n",
    );
    let jobs = scheduled_jobs(&config);
    assert!(job(&jobs, "youtube").enabled);
    assert!(job(&jobs, "todo-remind").enabled);
    assert_eq!(job(&jobs, "youtube").interval_secs, Some(3600));
}

#[test]
fn exposes_poll_and_run_commands() {
    let jobs = scheduled_jobs(&load(""));
    let youtube = job(&jobs, "youtube");
    assert_eq!(youtube.poll_command, "youtube poll");
    assert_eq!(youtube.run_command, "youtube run");
    let remind = job(&jobs, "todo-remind");
    assert_eq!(remind.poll_command, "todo remind --once");
    assert_eq!(remind.run_command, "todo remind");
    assert_eq!(remind.module, "planner");
}
