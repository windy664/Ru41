//! 活动总览 markdown 渲染：会议/论坛在前、直播/讲座在后，各自按开始时间升序；
//! 纯函数便于测试。

use crate::filter::EventRow;
use chrono::{Datelike, NaiveDate, TimeZone};

pub fn render(rows: &[EventRow], today: NaiveDate) -> String {
    let mut out = format!(
        "# 🗓️ 活动日程总览\n\n\
         > 生成于 {today} · 数据源：茶思屋会议日历，由 ru41 events 附属定时刷新\n\
         > 口径：科技向（AI/网络安全/软件/开源/芯片/计算等），已结束与非科技方向自动滤掉；\n\
         > ⭐ = 大湾区线下；全国性重大大会（WAIC/GOSIM/CNCC 等）不论地域都会推送通知\n\n"
    );
    let conferences: Vec<&EventRow> = rows.iter().filter(|r| r.kind == "会议/论坛").collect();
    let lives: Vec<&EventRow> = rows.iter().filter(|r| r.kind == "直播/讲座").collect();
    out.push_str(&format!("## 会议/论坛（{}）\n\n", conferences.len()));
    push_section(&mut out, &conferences);
    out.push_str(&format!("\n## 直播/讲座（{}）\n\n", lives.len()));
    push_section(&mut out, &lives);
    out
}

fn push_section(out: &mut String, rows: &[&EventRow]) {
    if rows.is_empty() {
        out.push_str("（空）\n");
        return;
    }
    for row in rows {
        let day = chrono::Local
            .timestamp_opt(row.start, 0)
            .single()
            .map(|t| t.format("%m-%d").to_string())
            .unwrap_or_else(|| "?".into());
        let weekday = chrono::Local
            .timestamp_opt(row.start, 0)
            .single()
            .map(|t| weekday_zh(t.weekday()))
            .unwrap_or("");
        let domains = if row.domains.is_empty() {
            String::new()
        } else {
            format!("｜{}", row.domains.join("·"))
        };
        let star = if row.gba { "⭐ " } else { "" };
        let cell = |s: &str| -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") };
        out.push_str(&format!(
            "- **{day} {weekday}** {star}[{}]({})｜{}{domains}\n",
            cell(&row.title),
            row.url,
            cell(&row.place),
        ));
    }
}

fn weekday_zh(day: chrono::Weekday) -> &'static str {
    match day {
        chrono::Weekday::Mon => "周一",
        chrono::Weekday::Tue => "周二",
        chrono::Weekday::Wed => "周三",
        chrono::Weekday::Thu => "周四",
        chrono::Weekday::Fri => "周五",
        chrono::Weekday::Sat => "周六",
        chrono::Weekday::Sun => "周日",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_conferences_before_lives() {
        let rows = vec![
            EventRow {
                id: "1".into(),
                title: "某直播讲座".into(),
                start: 1_793_600_000,
                end: None,
                place: "线上".into(),
                kind: "直播/讲座",
                gba: false,
                major: false,
                domains: vec!["人工智能领域".into()],
                url: "https://example.com/1".into(),
            },
            EventRow {
                id: "2".into(),
                title: "某论坛".into(),
                start: 1_793_600_000,
                end: None,
                place: "广州".into(),
                kind: "会议/论坛",
                gba: false,
                major: false,
                domains: Vec::new(),
                url: "https://example.com/2".into(),
            },
        ];
        let out = render(&rows, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        let conf = out.find("某论坛").unwrap();
        let live = out.find("某直播讲座").unwrap();
        assert!(conf < live);
        assert!(out.contains("会议/论坛（1）"));
        assert!(out.contains("直播/讲座（1）"));
    }
}
