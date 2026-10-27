//! BOSS 直聘 sidecar（sidecar/boss-crawler）抓取结果 → JobEntry。
//! sidecar 把岗位写到 boss_jobs.json（encryptJobId 去重、14 天未再出现的剔除），
//! jobs 模块 poll 时读进来与面灵条目合并。读失败/文件不存在只降级记日志。

use super::entry::{JobEntry, filter_entries};
use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct BossJobsFile {
    jobs: Vec<BossJob>,
}

#[derive(Debug, Deserialize)]
struct BossJob {
    #[serde(rename = "encryptJobId")]
    encrypt_job_id: String,
    job_name: String,
    company: String,
    #[serde(default)]
    city: String,
    #[serde(default)]
    experience: String,
    #[serde(default)]
    degree: String,
    /// JD 全文（详情接口 postDescription）；详情没抓到时为空串。
    #[serde(default)]
    jd: String,
    /// 首次见到日期 "YYYY-MM-DD"。
    #[serde(default)]
    first_seen: String,
}

/// "2026-10-26" → "10月26日"（与 posted_md 解析兼容）；解析不了回 "未公布"。
fn iso_date_to_md(iso: &str) -> String {
    chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d")
        .map(|d| {
            format!(
                "{}月{}日",
                d.format("%m").to_string().trim_start_matches('0'),
                d.format("%d").to_string().trim_start_matches('0')
            )
        })
        .unwrap_or_else(|_| "未公布".into())
}

/// 经验/学历拼招聘对象，如 "经验不限 · 本科"；都空回 "未公布"。
fn audience_of(experience: &str, degree: &str) -> String {
    let parts: Vec<&str> = [experience, degree]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if parts.is_empty() {
        "未公布".into()
    } else {
        parts.join(" · ")
    }
}

/// 读 boss_jobs.json → 技术岗口径筛选后的 JobEntry。
/// BOSS 侧只搜实习关键词，kind 一律 "实习"；JD 详情接口直接带全文，jd_tried=true。
pub fn load_entries(path: &str) -> Result<Vec<JobEntry>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("boss_jobs read: {path}"))?;
    let file: BossJobsFile = serde_json::from_str(&text).context("boss_jobs parse")?;
    Ok(filter_entries(
        file.jobs
            .into_iter()
            .filter(|j| !j.encrypt_job_id.is_empty() && !j.job_name.is_empty())
            .map(|j| JobEntry {
                company: j.company,
                role: j.job_name,
                city: if j.city.is_empty() {
                    "未公布".into()
                } else {
                    j.city
                },
                audience: audience_of(&j.experience, &j.degree),
                kind: "实习".into(),
                posted: iso_date_to_md(&j.first_seen),
                platform: Some("boss".into()),
                jd: (!j.jd.is_empty()).then_some(j.jd),
                jd_tried: true,
                link: Some(format!(
                    "https://www.zhipin.com/job_detail/{}.html",
                    j.encrypt_job_id
                )),
                link_tried: true,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    },
    {
      "encryptJobId": "def456",
      "job_name": "后端开发实习生（Go）",
      "company": "汇聚科技",
      "city": "广州",
      "experience": "在校/应届",
      "degree": "",
      "jd": "",
      "first_seen": "2026-10-24"
    },
    {
      "encryptJobId": "ghi789",
      "job_name": "AI产品运营实习生",
      "company": "某公司",
      "city": "深圳",
      "first_seen": "2026-10-25"
    }
  ]
}"#;

    #[test]
    fn parses_and_filters_fixture() {
        let dir = std::env::temp_dir().join(format!("boss-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("boss_jobs.json");
        std::fs::write(&path, FIXTURE).unwrap();

        let entries = load_entries(path.to_str().unwrap()).unwrap();
        // "AI产品运营实习生" 命中剔除词（运营），被技术岗口径滤掉。
        assert_eq!(entries.len(), 2);
        let first = &entries[0];
        assert_eq!(first.role, "大模型算法实习生");
        assert_eq!(first.platform.as_deref(), Some("boss"));
        assert_eq!(first.kind, "实习");
        assert_eq!(first.posted, "10月25日");
        assert_eq!(first.audience, "经验不限 · 本科");
        assert_eq!(first.jd.as_deref(), Some("参与大模型训练与评测……"));
        assert!(first.jd_tried);

        let second = entries
            .iter()
            .find(|e| e.role.contains("后端开发"))
            .unwrap();
        assert_eq!(second.audience, "在校/应届");
        assert!(second.jd.is_none(), "空 JD 不落成 Some");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_err() {
        assert!(load_entries("/nonexistent/boss_jobs.json").is_err());
    }
}
