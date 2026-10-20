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
    let report: ZhihuishuReport = serde_json::from_str(&raw)
        .with_context(|| format!("解析智慧树报告失败: {}", path.display()))?;
    Ok(report)
}

/// 平台时间 "YYYY-MM-DD HH:MM:SS"（按本机时区）→ Unix 秒。
pub fn parse_platform_time(value: &str) -> Option<i64> {
    let naive = chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    chrono::Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp())
}
