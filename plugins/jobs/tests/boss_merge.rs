//! BOSS 条目与面灵条目的合并渲染集成（原 adapters/sources/jobs/boss.rs 内测）。

use ru41::adapters::sources::jobs::{boss::load_entries, entry::JobEntry, mianling::MianlingJob};
use ru41_plugin_jobs::filter::{JobsState, filter_jobs, merge};
use ru41_plugin_jobs::overview::render;

const FIXTURE: &str = r#"{
  "updated_at": "2026-10-26T09:00:00",
  "jobs": [
    {
      "encryptJobId": "abc123",
      "job_name": "大模型算法实习生",
      "company": "星辰智能",
      "city": "深圳",
      "salary": "300-400元/天",
      "experience": "经验不限",
      "degree": "本科",
      "skills": ["PyTorch", "LLM"],
      "jd": "参与大模型训练与评测……",
      "first_seen": "2026-10-25",
      "last_seen": "2026-10-26"
    }
  ]
}"#;

#[test]
fn boss_entries_merge_and_render_with_mianling() {
    let dir = std::env::temp_dir().join(format!("boss-merge-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("boss_jobs.json");
    std::fs::write(&path, FIXTURE).unwrap();
    let boss = load_entries(path.to_str().unwrap()).unwrap();

    let mianling = filter_jobs(vec![MianlingJob {
        company: "美团".into(),
        role: "数据开发工程师".into(),
        city: "北京".into(),
        audience: "未公布".into(),
        kind: "校招".into(),
        posted: "10月26日".into(),
        platform: Some("meituan".into()),
    }]);

    let mut state = JobsState::default();
    merge(&mut state, mianling, (10, 26));
    merge(&mut state, boss, (10, 26));
    assert_eq!(state.entries.len(), 2);

    let rows: Vec<JobEntry> = state.entries.values().cloned().collect();
    let md = render(
        &rows,
        chrono::NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(),
    );
    assert!(md.contains("## 实习（1 条）"));
    assert!(md.contains("## 校招（1 条）"));
    assert!(md.contains("⭐ 深圳"));
    assert!(md.contains(
        "### [星辰智能 · 大模型算法实习生](https://www.zhipin.com/job_detail/abc123.html)"
    ));

    std::fs::remove_dir_all(&dir).ok();
}
