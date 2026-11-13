//! 智慧树 sidecar 报告文件的读取与解析。
//!
//! 报告由 zhihuishu-sidecar 采集器产出（Docker 容器内登录后读取课程与任务列表），
//! schema 契约见 docs/sources-zhihuishu.md；本模块只消费文件，不接触智慧树网络协议。

use anyhow::{Context, Result};
use chrono::TimeZone;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ZhihuishuReport {
    /// 报告生成时间（sidecar 本地时区）。
    #[serde(default)]
    pub generated_at: Option<String>,
    /// sidecar 登录态是否有效；false 时需要人工重新登录。
    #[serde(default)]
    pub login_ok: Option<bool>,
    #[serde(default)]
    pub courses: Vec<ZhihuishuCourse>,
}

#[derive(Debug, Deserialize)]
pub struct ZhihuishuCourse {
    pub title: String,
    /// polymas（新版 AI 教学中心）或 wenda（老平台）。
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub course_home_url: Option<String>,
    /// 视频内容是否已发布：Some(true)=可自动学习，Some(false)=暂无内容，
    /// None=无法判断（wenda 平台或页面结构特殊）。
    #[serde(default)]
    pub content_published: Option<bool>,
    #[serde(default)]
    pub tasks: Vec<ZhihuishuTask>,
}

#[derive(Debug, Deserialize)]
pub struct ZhihuishuAttachment {
    /// 附件显示名（sidecar 已做文件名净化）。
    pub name: String,
    /// 相对 sidecar 数据根的路径：attachments/{task_id}/{filename}。
    pub file: String,
}

#[derive(Debug, Deserialize)]
pub struct ZhihuishuTask {
    pub activity_task_id: String,
    pub title: String,
    /// assignment（题目作业）/ group（小组作业）/ exam（考试式）/ other。
    #[serde(default)]
    pub kind: Option<String>,
    /// 平台本地时间 "YYYY-MM-DD HH:MM:SS"。
    #[serde(default)]
    pub start_at: Option<String>,
    #[serde(default)]
    pub end_at: Option<String>,
    #[serde(default)]
    pub completed: Option<bool>,
    #[serde(default)]
    pub total_score: Option<f64>,
    /// 限时作答分钟数；>0 表示考试式（红线：一律人工，不进入草稿管线）。
    #[serde(default)]
    pub exam_duration_minutes: Option<i64>,
    /// 摄像头监考标记；true 表示考试式。
    #[serde(default)]
    pub video_invigilate: Option<bool>,
    /// 是否允许迟交。
    #[serde(default)]
    pub late_submit: Option<bool>,
    /// 作业描述文本。
    #[serde(default)]
    pub content: Option<String>,
    /// 平台附件（sidecar 已下载到 attachments_dir，file 为相对其根的路径）。
    #[serde(default)]
    pub attachments: Option<Vec<ZhihuishuAttachment>>,
}

/// 读取并解析报告文件；文件缺失或 JSON 损坏都返回错误（只影响本轮轮询）。
pub fn load_report(path: &Path) -> Result<ZhihuishuReport> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("读取智慧树报告失败: {}", path.display()))?;
    let mut report: ZhihuishuReport = serde_json::from_str(&raw)
        .with_context(|| format!("解析智慧树报告失败: {}", path.display()))?;
    normalize_report_times(&mut report);
    Ok(report)
}

/// 报告里的 start_at/end_at 统一归一化到契约格式（原地改写）。
/// 归一化不出来的值保留原样，不丢数据。
fn normalize_report_times(report: &mut ZhihuishuReport) {
    for course in &mut report.courses {
        for task in &mut course.tasks {
            if let Some(v) = task.end_at.as_deref().and_then(normalize_platform_time) {
                task.end_at = Some(v);
            }
            if let Some(v) = task.start_at.as_deref().and_then(normalize_platform_time) {
                task.start_at = Some(v);
            }
        }
    }
}

/// 平台时间归一化：契约要求 "YYYY-MM-DD HH:MM:SS"（本机时区）。
///
/// 智慧树的「全局待办」接口(findNoFinishTaskTotal)的 startTime/endTime 返回毫秒
/// epoch 串（如 "1794628800000"），课程流返回的是 ISO 串——两种形态不同源。
/// sidecar 侧已做归一化，这里再兜一层：老报告 / 老快照里可能残留毫秒串，
/// 不归一化会让「截止变更」误报（同一时刻的两种写法被当成变更），
/// 并使 parse_platform_time 解析失败——截止/临期判定与待办 due_at 一起失效。
///
/// 返回 None 表示无法识别，调用方应保留原值而不是丢弃。
pub fn normalize_platform_time(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    for fmt in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    // 纯日期：chrono 的 NaiveDateTime 解析器不接受缺时间的串，单独走 NaiveDate。
    if let Ok(date) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(
            date.and_hms_opt(0, 0, 0)
                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| trimmed.to_owned()),
        );
    }
    // 纯数字 → 秒/毫秒 epoch。只认 2001-09-09 ~ 2100-01-01 区间，
    // 区间外的紧凑日期串（如 "20261114"）不当 epoch，原样保留。
    if !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let Ok(n) = trimmed.parse::<i64>() else {
        return None;
    };
    if !(1_000_000_000..=4_102_444_800_000).contains(&n) {
        return None;
    }
    let secs = if n >= 100_000_000_000 { n / 1000 } else { n };
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
        dt.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    })
}

/// 平台时间 "YYYY-MM-DD HH:MM:SS"（按本机时区）→ Unix 秒。
/// 兜底接受毫秒/秒 epoch 串（历史报告或快照里可能残留）。
pub fn parse_platform_time(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return chrono::Local
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| dt.timestamp());
    }
    let canonical = normalize_platform_time(trimmed)?;
    let naive = chrono::NaiveDateTime::parse_from_str(&canonical, "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}
