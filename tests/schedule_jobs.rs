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
    let jobs = scheduled_jobs(&load(""));
    for name in [
        "papers",
        "rss",
        "youtube",
        "ecard",
        "compshare",
        "zhihuishu",
        "todo-remind",
    ] {
        assert!(jobs.iter().any(|j| j.name == name), "{name} missing");
    }
}

#[test]
fn reports_disabled_without_config() {
    let jobs = scheduled_jobs(&load(""));
    assert!(!job(&jobs, "papers").enabled);
    assert!(!job(&jobs, "rss").enabled);
    assert!(!job(&jobs, "youtube").enabled);
    assert!(!job(&jobs, "ecard").enabled);
    assert!(!job(&jobs, "compshare").enabled);
    // 智慧树报告路径有默认值，登记即可启动。
    assert!(job(&jobs, "zhihuishu").enabled);
    assert!(!job(&jobs, "todo-remind").enabled);
    assert!(job(&jobs, "papers").notes.is_some());
}

#[test]
fn flips_enabled_when_configured() {
    let config = load(
        "[sources.arxiv]\ncategories = [\"cs.CR\"]\ninterval_secs = 3600\n\
         [sources.rss]\nfeeds = [\"https://example.com/f.xml\"]\n\
         [sources.youtube]\nchannels = [\"@test\"]\n\
         [sources.ecard]\nbase_url = \"https://ecard.example.edu.cn\"\ntoken = \"t\"\n\
         impl_type = \"I\"\nschool_area_no = \"1\"\nbuilding_no = \"2\"\nroom_num = \"3\"\n\
         [sources.compshare]\nproject_id = \"org-test\"\ncookie = \"c\"\ninterval_secs = 60\n\
         [notify]\nchannel = \"qq\"\n[channel.qq]\ndefault_notify_target = \"user:U1\"\n",
    );
    let jobs = scheduled_jobs(&config);
    assert!(job(&jobs, "papers").enabled);
    assert!(job(&jobs, "rss").enabled);
    assert!(job(&jobs, "youtube").enabled);
    assert!(job(&jobs, "ecard").enabled);
    assert!(job(&jobs, "compshare").enabled);
    assert!(job(&jobs, "todo-remind").enabled);
    assert_eq!(job(&jobs, "papers").interval_secs, Some(3600));
    assert_eq!(job(&jobs, "ecard").interval_secs, Some(86_400));
    assert_eq!(job(&jobs, "compshare").interval_secs, Some(60));
}

#[test]
fn exposes_poll_and_run_commands() {
    let jobs = scheduled_jobs(&load(""));
    let papers = job(&jobs, "papers");
    assert_eq!(papers.poll_command, "papers poll");
    assert_eq!(papers.run_command, "papers run");
    let remind = job(&jobs, "todo-remind");
    assert_eq!(remind.poll_command, "todo remind --once");
    assert_eq!(remind.run_command, "todo remind");
    assert_eq!(remind.module, "planner");
}
