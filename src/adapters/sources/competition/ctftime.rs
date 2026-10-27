//! CTFtime（ctftime.org）官方公开 API：全球 CTF 赛事日历，国内大赛
//! （强网杯 / TCTF / XCTF 系列 / 网鼎杯等）赛季都会登记在此。
//! 无需 key，按时间窗拉取即将开始的赛事。CTF 无"报名截止"字段，
//! 以开赛时间作为行动节点；奖金常为 TBD/TBA，质量用 weight（联赛权重）
//! 兜底——weight ≥ 20 是公认大赛线（强网杯/TCTF 在 50+）。

use anyhow::{Context, Result};

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CtftimeEvent {
    pub id: u64,
    pub title: String,
    /// 赛事官网（可能为空）。
    pub url: String,
    /// CTFtime 赛事页（必定存在）。
    pub ctftime_url: String,
    pub onsite: bool,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub format: String,
    #[serde(default)]
    pub prizes: String,
    pub start: String,
    pub finish: String,
    #[serde(default)]
    pub weight: f64,
}

pub struct CtftimeClient {
    http: reqwest::Client,
}

impl Default for CtftimeClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CtftimeClient {
    pub fn new() -> Self {
        Self {
            // ctftime 拦非浏览器 UA
            http: reqwest::Client::builder()
                .user_agent(
                    "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0",
                )
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("ctftime client build"),
        }
    }

    /// 拉取未来 window_days 内开赛的赛事（含正在进行中的）。
    pub async fn fetch_events(&self, window_days: i64) -> Result<Vec<CtftimeEvent>> {
        let now = chrono::Utc::now().timestamp();
        let finish = now + window_days * 86400;
        let url =
            format!("https://ctftime.org/api/v1/events/?limit=100&start={now}&finish={finish}");
        let body =
            crate::adapters::sources::feed::fetch_text(self.http.get(url), "ctftime").await?;
        serde_json::from_str(&body).context("ctftime json parse failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_api_response() {
        let body = r#"[{"id":3444,"title":"XCTF Finals 2026","url":"https://adworld.xctf.org.cn",
            "ctftime_url":"https://ctftime.org/event/3444/","onsite":false,"location":"",
            "format":"Jeopardy","prizes":"TBD","weight":50.0,
            "start":"2026-11-06T09:00:00+00:00","finish":"2026-11-08T09:00:00+00:00"}]"#;
        let events: Vec<CtftimeEvent> = serde_json::from_str(body).unwrap();
        assert_eq!(events[0].title, "XCTF Finals 2026");
        assert!((events[0].weight - 50.0).abs() < f64::EPSILON);
        assert!(!events[0].onsite);
    }
}
