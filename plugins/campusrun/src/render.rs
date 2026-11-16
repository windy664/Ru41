//! 把控制台数据渲染成人读的 Markdown。
//!
//! 纯函数，不触网，方便单测锁住「用户到底会看到什么」。

use crate::model::{CheatStatus, RunRecord, SemesterInfo};
use chrono::{Local, TimeZone};

/// 学期进度。
pub fn render_status(info: &SemesterInfo) -> String {
    let summary = &info.summary_raw;
    let mut lines = vec!["## 校园跑进度".to_owned()];

    let name = summary
        .sname
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("（学期名未提供）");
    lines.push(format!("- 学期：{name}"));

    let total = summary.semester_dis.unwrap_or_default();
    let valid = summary.semester_valid_dis.unwrap_or_default();
    let ratio = summary
        .valid_ratio
        .map(|r| format!("，有效率 {:.1}%", r * 100.0))
        .unwrap_or_default();
    lines.push(format!(
        "- 有效里程：{}（共 {}{}）",
        fmt_distance(valid),
        fmt_distance(total),
        ratio
    ));

    let total_count = summary.semester_count.unwrap_or_default();
    let valid_count = summary.semester_valid_count.unwrap_or_default();
    lines.push(format!(
        "- 有效次数：{valid_count} 次（共 {total_count} 次）"
    ));

    // 目标：次数与距离分列，别把「最小单次里程」当成学期总里程。
    let goal_count = info
        .goal_raw
        .as_ref()
        .and_then(|g| g.semester_goal)
        .filter(|n| *n > 0);
    let done_count = info
        .goal_raw
        .as_ref()
        .and_then(|g| g.semester_valid)
        .unwrap_or(valid_count);
    if let Some(target) = goal_count {
        let reached = done_count >= target;
        let tail = if reached {
            "已达标".to_owned()
        } else {
            format!("还差 {} 次，未达标", target - done_count)
        };
        lines.push(format!(
            "- 学期目标：{target} 次（已完成 {done_count} 次）→ {tail}"
        ));
    }
    if let Some(distance) = info.goal.as_ref().and_then(|g| g.target_dis) {
        lines.push(format!("- 目标距离：{:.2} km", distance));
    }
    if let Some(days) = info.goal.as_ref().and_then(|g| g.days_left) {
        lines.push(format!("- 学期剩余：{days} 天"));
    }
    if info.goal.as_ref().and_then(|g| g.in_term) == Some(false) {
        lines.push("- ⚠️ 当前不在学期内（成绩不计入本学期）".to_owned());
    }
    lines.join("\n")
}

/// 最近若干条记录。
pub fn render_records(records: &[RunRecord], limit: usize) -> String {
    let mut lines = vec!["## 校园跑最近记录".to_owned()];
    if records.is_empty() {
        lines.push("- 还没有记录".to_owned());
        return lines.join("\n");
    }
    let shown: Vec<&RunRecord> = records.iter().take(limit.max(1)).collect();
    lines.push("| 时间 | 距离 | 配速 | 有效 | 时长 | 步频 | 状态 |".to_owned());
    lines.push("| --- | --- | --- | --- | --- | --- | --- |".to_owned());
    for record in shown.iter() {
        let distance = record.total_dis.unwrap_or_default();
        let seconds = record.total_time.unwrap_or_default();
        let valid = record.valid_dis.unwrap_or(distance);
        let pace = pace_per_km(distance, seconds).unwrap_or_else(|| "—".to_owned());
        let valid_text = if valid + 0.5 < distance {
            format!("{}/{}", fmt_distance(valid), fmt_distance(distance))
        } else {
            fmt_distance(valid)
        };
        let step = record
            .avg_step_freq
            .filter(|n| *n > 0)
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".to_owned());
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            fmt_datetime(record.start_time),
            fmt_distance(distance),
            pace,
            valid_text,
            fmt_duration(seconds),
            step,
            record_state(record),
        ));
    }
    if records.len() > shown.len() {
        lines.push(format!(
            "\n共 {} 条，这里只列最近 {} 条。",
            records.len(),
            shown.len()
        ));
    }
    lines.join("\n")
}

/// 违规名单/风控状态。
pub fn render_cheat(cheat: &CheatStatus) -> String {
    let mut lines = vec!["## 校园跑账号状态".to_owned()];
    let label = cheat
        .label
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("（服务端未给标签）");
    lines.push(format!("- 违规名单：{label}（{}）", cheat.status));
    if let Some(count) = cheat.count {
        lines.push(format!("- 违规条数：{count}"));
    }
    if let Some(list) = cheat.list_count {
        lines.push(format!("- 服务端通报条数：{list}"));
    }
    if let Some(reason) = cheat.reason.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- 原因：{reason}"));
    }
    if let Some(appeal) = cheat.appeal.as_deref().filter(|s| !s.trim().is_empty()) {
        lines.push(format!("- 申诉：{appeal}"));
    }
    if let Some(occurred) = cheat
        .occurred_at
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        lines.push(format!("- 时间：{occurred}"));
    }
    // 「未知」不等于清白——报成清白比不报更糟。
    if cheat.status == "unknown" {
        lines.push("- ⚠️ 服务端字段缺失或异常，**不等于清白**，请到 App 里核对".to_owned());
    }
    if cheat.status == "stale" {
        lines.push("- ⚠️ 查询结果已过期（超过 5 分钟），请重查".to_owned());
    }
    lines.push(format!("- 查询时间：{}", fmt_datetime(cheat.checked_at_ms)));
    lines.join("\n")
}

/// 一页看完：进度 + 最近记录 + 账号状态。
pub fn render_all(status: &SemesterInfo, records: &[RunRecord], cheat: &CheatStatus) -> String {
    format!(
        "{}\n\n{}\n\n{}",
        render_status(status),
        render_records(records, 5),
        render_cheat(cheat)
    )
}

/// 记录状态文案。
fn record_state(record: &RunRecord) -> String {
    let mut parts = vec![if record.complete.unwrap_or(false) {
        "已完成".to_owned()
    } else {
        "未完成".to_owned()
    }];
    if let Some(status) = record.status.filter(|s| *s != 0) {
        parts.push(format!("服务端状态异常({status})"));
    }
    if record.appeal_status.unwrap_or(0) != 0 {
        parts.push("有申诉记录".to_owned());
    }
    if record.step_freq_status.unwrap_or(0) != 0 {
        parts.push("步频被判异常".to_owned());
    }
    parts.join("，")
}

/// 米 → km。
pub fn fmt_distance(meters: f64) -> String {
    format!("{:.2} km", meters / 1000.0)
}

/// 秒 → 配速（min/km）。
pub fn pace_per_km(distance_m: f64, seconds: f64) -> Option<String> {
    if distance_m <= 0.0 || seconds <= 0.0 {
        return None;
    }
    let pace_seconds = (seconds / (distance_m / 1000.0)).round() as i64;
    Some(format!(
        "{}'{:02}\"/km",
        pace_seconds / 60,
        pace_seconds % 60
    ))
}

/// 秒 → 时长。
pub fn fmt_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as i64;
    format!("{}'{:02}\"", total / 60, total % 60)
}

/// 毫秒时间戳 → `MM-DD HH:MM`。
pub fn fmt_datetime(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => match Local.timestamp_millis_opt(ms).single() {
            Some(time) => time.format("%m-%d %H:%M").to_string(),
            None => "—".to_owned(),
        },
        None => "—".to_owned(),
    }
}

/// 毫秒时间戳 → `YYYY-MM-DD`（学期起止用）。
pub fn fmt_date(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => match Local.timestamp_millis_opt(ms).single() {
            Some(time) => time.format("%Y-%m-%d").to_string(),
            None => "—".to_owned(),
        },
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{SemesterGoal, SemesterGoalRaw, SemesterSummary};

    fn summary() -> SemesterSummary {
        SemesterSummary {
            semester_dis: Some(7511.0),
            semester_valid_dis: Some(6295.0),
            sname: Some("2026-2027-1体能训练Ⅰ".into()),
            semester_count: Some(0),
            semester_valid_count: Some(0),
            valid_ratio: Some(0.8381041139661829),
            text: Some("服务端自己拼的那句话".into()),
        }
    }

    fn info() -> SemesterInfo {
        SemesterInfo {
            summary_raw: summary(),
            goal_raw: Some(SemesterGoalRaw {
                semester_goal: Some(1),
                semester_valid: Some(0),
                goal_method: Some(2),
                start_date: Some(1_793_376_000_000),
                end_date: Some(1_798_819_199_000),
                sname: None,
            }),
            goal: Some(SemesterGoal {
                target_dis: Some(1.0),
                target_count: Some(0),
                days_left: Some(45),
                in_term: Some(true),
            }),
        }
    }

    #[test]
    fn status_shows_goal_count_not_cli_zero() {
        let text = render_status(&info());
        // 目标次数来自 goalRaw（1 次），不是 pace-forge CLI 打出来的 0 次。
        assert!(text.contains("学期目标：1 次"), "{text}");
        assert!(text.contains("还差 1 次，未达标"), "{text}");
        assert!(text.contains("有效里程：6.29 km"), "{text}");
        assert!(text.contains("共 7.51 km"), "{text}");
        assert!(text.contains("目标距离：1.00 km"), "{text}");
        assert!(text.contains("学期剩余：45 天"), "{text}");
        // 服务端那句自拼摘要不直接照抄（它可能和目标字段打架）。
        assert!(!text.contains("服务端自己拼的那句话"), "{text}");
    }

    #[test]
    fn pace_and_duration_format() {
        assert_eq!(pace_per_km(1212.0, 426.0).as_deref(), Some("5'51\"/km"));
        assert_eq!(pace_per_km(0.0, 100.0), None);
        assert_eq!(fmt_duration(426.0), "7'06\"");
        assert_eq!(fmt_distance(1212.0), "1.21 km");
    }

    #[test]
    fn record_state_flags_anomalies() {
        let mut record = RunRecord {
            rrid: 1,
            uuid: None,
            sport_type: Some(1),
            total_dis: Some(1212.0),
            total_time: Some(426.0),
            valid_dis: Some(1212.0),
            valid_time: Some(426.0),
            start_time: Some(1_794_870_446_000),
            stop_time: None,
            complete: Some(true),
            status: Some(0),
            appeal_status: Some(0),
            step_freq_status: Some(0),
            avg_step_freq: Some(158),
            distance_km: Some(1.21),
        };
        assert_eq!(record_state(&record), "已完成");
        record.status = Some(1);
        assert!(record_state(&record).contains("服务端状态异常(1)"));
        record.appeal_status = Some(2);
        assert!(record_state(&record).contains("有申诉记录"));
    }

    #[test]
    fn cheat_unknown_is_not_reported_as_clean() {
        let cheat = CheatStatus {
            status: "unknown".into(),
            label: Some("状态未知".into()),
            count: None,
            reason: None,
            appeal: None,
            occurred_at: None,
            rrid: None,
            checked_at_ms: Some(1_794_871_590_236),
            list_count: Some(9),
        };
        let text = render_cheat(&cheat);
        assert!(text.contains("不等于清白"), "{text}");
        assert!(text.contains("通报条数：9"), "{text}");
    }

    #[test]
    fn records_table_lists_recent_rows() {
        let records = vec![RunRecord {
            rrid: 1367213418,
            uuid: Some("u".into()),
            sport_type: Some(1),
            total_dis: Some(1212.0),
            total_time: Some(426.0),
            valid_dis: Some(1000.0),
            valid_time: Some(400.0),
            start_time: Some(1_794_870_446_000),
            stop_time: Some(1_794_870_872_000),
            complete: Some(true),
            status: Some(0),
            appeal_status: Some(0),
            step_freq_status: Some(0),
            avg_step_freq: Some(158),
            distance_km: Some(1.21),
        }];
        let text = render_records(&records, 5);
        assert!(
            text.contains("| 11-17 07:07 | 1.21 km | 5'51\"/km |"),
            "{text}"
        );
        // 有效 < 总：两个数都显示，别让人误以为整条都算。
        assert!(text.contains("1.00 km/1.21 km"), "{text}");
    }
}
