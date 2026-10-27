//! Google Summer of Code（summerofcode.withgoogle.com）：官方公开 JSON API
//! （/api/program/{year}/，免认证），含整季 milestones 时间线——公告/组织申请/
//! contributor 提案/结项等节点齐全。/api/program/current/ 会 404，所以按
//! 年份探测：先试明年再试今年，取 is_active 的一季。在 rss 模块里以
//! `gsoc+https://summerofcode.withgoogle.com/` 前缀接入（URL 仅作标识）。
//! 只产出未来 120 天内的里程碑；申请窗口期额外补一条「申请进行中」。

use super::super::feed::{FeedItem, fetch_text};
use anyhow::{Context, Result};

/// rss feed 列表里的接入前缀：`gsoc+https://summerofcode.withgoogle.com/`。
pub const URL_PREFIX: &str = "gsoc+";

/// 注册表条目：`gsoc+<标识-url>` 路由到本源。
pub const DEF: super::super::SourceDef = super::super::SourceDef {
    prefix: URL_PREFIX,
    fetch: |url| Box::pin(async move { GsocClient::new().fetch_program(&url).await }),
};

const API_BASE: &str = "https://summerofcode.withgoogle.com/api/program";
/// 只看未来 120 天内的里程碑，更远的等临近再进池。
const HORIZON_SECS: i64 = 120 * 86_400;

pub struct GsocClient {
    http: reqwest::Client,
}

/// 类型化 program 数据：年份/is_active/phase + 里程碑时间线。
pub struct GsocProgram {
    pub year: String,
    pub is_active: bool,
    pub phase: String,
    pub milestones: Vec<Milestone>,
}

pub struct Milestone {
    /// API 的 name 字段（如 program_end），跨项目规模会重复，不用作 id。
    pub label: String,
    /// ISO 时间原文（如 2026-11-11T18:00:00Z）。
    pub datetime: String,
    /// datetime 解析出的 Unix 秒。
    pub ts: i64,
}

impl Default for GsocClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GsocClient {
    pub fn new() -> Self {
        Self {
            http: crate::util::http_client(),
        }
    }

    /// 探测明年→今年，取第一个存在且 is_active 的 program（结构化数据，
    /// 供科研机会总览等需要完整时间线的消费方使用）。
    pub async fn fetch_active_program(&self) -> Result<Option<GsocProgram>> {
        let now = crate::util::now();
        let this_year: i32 = crate::util::today_utc(now)[..4]
            .parse()
            .context("gsoc year derive failed")?;
        for year in [this_year + 1, this_year] {
            let api = format!("{API_BASE}/{year}/");
            let body = match fetch_text(self.http.get(&api), "gsoc").await {
                Ok(b) => b,
                Err(_) => continue, // 未来年份 program 未发布时 404
            };
            let program = parse_program(&body).context("gsoc json parse failed")?;
            if program.is_active {
                return Ok(Some(program));
            }
        }
        Ok(None)
    }

    /// 探测明年→今年，取第一个存在且 is_active 的 program，映射成 FeedItem。
    pub async fn fetch_program(&self, _url: &str) -> Result<Vec<FeedItem>> {
        let now = crate::util::now();
        Ok(match self.fetch_active_program().await? {
            Some(program) => program_items(&program, now),
            None => vec![],
        })
    }
}

/// 解析 /api/program/{year}/：顶层 slug/is_active/phase + milestones 数组。
fn parse_program(body: &str) -> Result<GsocProgram> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    let year = v
        .get("slug")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_owned();
    let mut milestones = vec![];
    for m in v
        .get("milestones")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
    {
        let label = m.get("label").and_then(|s| s.as_str()).unwrap_or("").trim();
        let datetime = m.get("datetime").and_then(|s| s.as_str()).unwrap_or("");
        let Some(ts) = chrono::DateTime::parse_from_rfc3339(datetime)
            .ok()
            .map(|d| d.timestamp())
        else {
            continue;
        };
        if label.is_empty() {
            continue;
        }
        milestones.push(Milestone {
            label: label.to_owned(),
            datetime: datetime.to_owned(),
            ts,
        });
    }
    Ok(GsocProgram {
        year,
        is_active: v
            .get("is_active")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        phase: v
            .get("phase")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_owned(),
        milestones,
    })
}

/// label → 稳定 id 片段：小写、非字母数字折叠成 -。
fn label_slug(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_dash = true; // 去掉前导 -
    for ch in label.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// UTC 时间戳转 +08:00 的 YYYY-MM-DD HH:mm。
pub(crate) fn format_cn(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| {
            d.with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).expect("utc+8"))
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

/// 未来 120 天内的里程碑各出一条；contributor 申请窗口内额外出一条。
fn program_items(program: &GsocProgram, now: i64) -> Vec<FeedItem> {
    let mut items = vec![];
    for m in &program.milestones {
        if m.ts < now || m.ts > now + HORIZON_SECS {
            continue;
        }
        items.push(FeedItem {
            id: format!("gsoc-{}-{}", program.year, label_slug(&m.label)),
            title: format!("GSoC {}：{}", program.year, m.label),
            link: "https://summerofcode.withgoogle.com/".to_owned(),
            summary: format!(
                "关键节点 {}，时间 {}。错过等一年，提前准备。",
                m.label,
                format_cn(m.ts)
            ),
            published: m.datetime.clone(),
        });
    }
    // 申请窗口判定：phase 名字不公开枚举，按里程碑区间推断更稳——
    // 现在落在 proposals open 与 deadline 之间即视为 contributor 申请进行中。
    let open = find_ts(program, "Contributor Proposals Open");
    let deadline = find_ts(program, "Proposal Deadline");
    if let (Some(open), Some(deadline)) = (open, deadline)
        && now >= open
        && now <= deadline
    {
        items.push(FeedItem {
            id: format!("gsoc-{}-contributor-applications-open", program.year),
            title: format!("GSoC {}：Contributor 申请进行中", program.year),
            link: "https://summerofcode.withgoogle.com/".to_owned(),
            summary: format!(
                "提案通道已开放，截止 {}。错过等一年，提前准备。",
                format_cn(deadline)
            ),
            published: chrono::DateTime::from_timestamp(now, 0)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
        });
    }
    items
}

fn find_ts(program: &GsocProgram, label: &str) -> Option<i64> {
    program
        .milestones
        .iter()
        .find(|m| m.label == label)
        .map(|m| m.ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "name": "2026",
      "slug": "2026",
      "is_active": true,
      "phase": "project_work",
      "milestones": [
        {"name": "prog_announce", "datetime": "2025-12-03T19:00:00Z", "label": "Program Announce Date"},
        {"name": "contributor_proposals_open", "datetime": "2026-03-16T18:00:00Z", "label": "Contributor Proposals Open"},
        {"name": "proposal_deadline", "datetime": "2026-03-31T18:00:00Z", "label": "Proposal Deadline"},
        {"name": "program_end", "datetime": "2026-11-11T18:00:00Z", "label": "Program End Date"},
        {"name": "project_completion", "datetime": "2027-08-31T18:00:00Z", "label": "Project Completion Date (medium project)"}
      ]
    }"#;

    // 2026-10-26T00:00:00Z
    const NOW: i64 = 1_792_972_800;

    #[test]
    fn parses_program_and_milestones() {
        let p = parse_program(FIXTURE).unwrap();
        assert_eq!(p.year, "2026");
        assert!(p.is_active);
        assert_eq!(p.phase, "project_work");
        assert_eq!(p.milestones.len(), 5);
        assert_eq!(p.milestones[3].label, "Program End Date");
    }

    #[test]
    fn emits_only_milestones_within_120_days() {
        let p = parse_program(FIXTURE).unwrap();
        let items = program_items(&p, NOW);
        // 过去的（announce/proposals/deadline）与 120 天外的（completion 2027-08）都不出；
        // 只剩 Program End Date（2026-11-11，距今 16 天）。申请窗口已过，无额外条目。
        assert_eq!(items.len(), 1);
        let it = &items[0];
        assert_eq!(it.id, "gsoc-2026-program-end-date");
        assert_eq!(it.title, "GSoC 2026：Program End Date");
        assert!(it.summary.contains("2026-11-12 02:00"), "{}", it.summary); // 18:00Z → +08:00
        assert_eq!(it.published, "2026-11-11T18:00:00Z");
    }

    #[test]
    fn emits_application_open_item_inside_window() {
        let p = parse_program(FIXTURE).unwrap();
        // 2026-03-20T00:00:00Z，在 proposals open(03-16) 与 deadline(03-31) 之间
        let now = 1_773_964_800;
        let items = program_items(&p, now);
        let extra = items
            .iter()
            .find(|i| i.id == "gsoc-2026-contributor-applications-open")
            .expect("窗口内应有申请进行中条目");
        assert!(
            extra.summary.contains("2026-04-01 02:00"),
            "{}",
            extra.summary
        );
    }

    #[test]
    fn label_slug_stable() {
        assert_eq!(
            label_slug("Midterm Evaluation Start (small project)"),
            "midterm-evaluation-start-small-project"
        );
        assert_eq!(label_slug("Program End Date"), "program-end-date");
    }
}
